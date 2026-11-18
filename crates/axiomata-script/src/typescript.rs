//! TypeScript made JavaScript for QuickJS: only the types go (oxc's TypeScript transform); nothing is lowered, since
//! QuickJS speaks current JavaScript.

use oxc::allocator::Allocator;
use oxc::codegen::Codegen;
use oxc::diagnostics::OxcDiagnostic;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::SourceType;
use oxc::transformer::{TransformOptions, Transformer};
use tokio::sync::oneshot;

/// The name the transform reports its errors under; no file is read.
const SOURCE_NAME: &str = "script.ts";
/// How many of oxc's messages an error carries: the first ones say what is wrong, the rest follow from them.
const MAX_MESSAGES: usize = 3;
/// The stack oxc parses on. Its parser and transform recurse with the source's nesting, which the script's writer
/// controls: on a thread of its own with this much (reserved, not used, until a deep source needs it), the deepest
/// script within [`crate::Limits::max_source_chars`] ends as an error, never as an overflow of the host's stack.
const STACK_BYTES: usize = 512 * 1024 * 1024;

/// The longest TypeScript script: oxc's memory grows with the square of a script's nesting (security audit
/// 2026-11-18: 100 000 characters of nested blocks took 2.5 GB), so TypeScript is held shorter than JavaScript.
const MAX_CHARS: usize = 20_000;
/// The deepest nesting of brackets oxc is given; real scripts stay far below it.
const MAX_NESTING: usize = 200;

/// How deep `source`'s brackets nest, strings and comments left aside — roughly: a template's `${` counts as a
/// bracket, which only errs towards refusing.
fn nesting(source: &str) -> usize {
    let mut deepest = 0;
    let mut depth: usize = 0;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' | '`' => {
                while let Some(inner) = chars.next() {
                    if inner == '\\' {
                        chars.next();
                    } else if inner == c {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                let mut last = ' ';
                for inner in chars.by_ref() {
                    if last == '*' && inner == '/' {
                        break;
                    }
                    last = inner;
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// `source`, a whole program in TypeScript, as JavaScript — worked out on a thread of its own.
///
/// # Errors
///
/// The parser's or the transform's first messages, each with its line, when the source is no valid TypeScript.
pub(crate) async fn strip(source: String) -> Result<String, String> {
    let (done, answer) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("script-typescript".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _ = done.send(strip_now(&source));
        });
    if let Err(err) = spawned {
        return Err(format!("no thread to read it on: {err}"));
    }
    answer
        .await
        .unwrap_or_else(|_| Err("reading it failed".into()))
}

fn strip_now(source: &str) -> Result<String, String> {
    if source.chars().count() > MAX_CHARS {
        return Err(format!(
            "a TypeScript script is at most {MAX_CHARS} characters"
        ));
    }
    if nesting(source) > MAX_NESTING {
        return Err(format!(
            "brackets nested deeper than {MAX_NESTING}: write it flatter"
        ));
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if parsed.fatal_error || !parsed.diagnostics.is_empty() {
        return Err(messages(source, parsed.diagnostics.iter()));
    }
    let mut program = parsed.program;
    let semantic = SemanticBuilder::new().with_enum_eval(true).build(&program);
    if !semantic.diagnostics.is_empty() {
        return Err(messages(source, semantic.diagnostics.iter()));
    }
    let scoping = semantic.semantic.into_scoping();
    let options = TransformOptions::default();
    let transformed = Transformer::new(&allocator, std::path::Path::new(SOURCE_NAME), &options)
        .build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        return Err(messages(source, transformed.diagnostics.iter()));
    }
    Ok(Codegen::new().build(&program).code)
}

/// The first messages, each with the line its first label points at — line 1 being the script's own first line.
fn messages<'a>(source: &str, all: impl Iterator<Item = &'a OxcDiagnostic>) -> String {
    let mut shown: Vec<String> = all
        .take(MAX_MESSAGES)
        .map(|diagnostic| match diagnostic.labels.first() {
            Some(label) => {
                let before = source.get(..label.offset() as usize).unwrap_or(source);
                let line = before.matches('\n').count() + 1;
                format!("line {line}: {}", diagnostic.message)
            }
            None => diagnostic.message.to_string(),
        })
        .collect();
    if shown.is_empty() {
        shown.push("the TypeScript could not be read".into());
    }
    shown.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn types_go_and_the_code_stays() {
        let out = strip(
            "interface Day { title: string }\n\
             const days: Day[] = [{ title: 'a' }];\n\
             (async (): Promise<void> => { const n = days.length as number; return; })();"
                .into(),
        )
        .await
        .unwrap();
        assert!(
            !out.contains("interface") && !out.contains(": Day") && !out.contains("as number"),
            "{out}"
        );
        assert!(out.contains("async"), "{out}");
    }

    #[tokio::test]
    async fn broken_typescript_says_why_and_where() {
        let err = strip("const a = 1;\nconst x: = 1;".into())
            .await
            .unwrap_err();
        assert!(err.starts_with("line 2: "), "{err}");
    }

    #[tokio::test]
    async fn deep_nesting_and_long_scripts_are_refused_before_oxc() {
        let blocks = format!("{}{}", "{".repeat(9_000), "}".repeat(9_000));
        assert!(
            strip(blocks)
                .await
                .unwrap_err()
                .starts_with("brackets nested deeper")
        );
        let long = "let a = 1;\n".repeat(3_000);
        assert!(
            strip(long)
                .await
                .unwrap_err()
                .starts_with("a TypeScript script is at most")
        );
        // Brackets in strings and comments do not count.
        assert_eq!(nesting("const s = '((((('; // ((((\n/* {{{{ */ f([{}])"), 3);
        let unary = format!("const x = {}1;", "-".repeat(15_000));
        let _ = strip(unary).await;
    }
}
