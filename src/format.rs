//! Formatting a file with its language's own formatter (`docs/plans/editor.md`,
//! ED6.5, L13) — the pattern of the owner's Neovim setup (conform.nvim):
//! `ruff`, `rustfmt`, `prettier`, `stylua`, `shfmt`, each with a line width of
//! 120 where it takes one. Where no formatter fits, the page falls back to the
//! language server's own formatting.
//!
//! As with the language servers ([`crate::lsp::servers`]), which program runs is
//! decided here: a built-in table, found on the search path, overridable only in
//! `~/.axiomata/lsp.json` (`"formatters"`), which no command writes. The page
//! sends a root, a path in it and the text; the text goes in on stdin and the
//! formatted text comes back on stdout. The file itself is never read or
//! written here — its path only tells a formatter which configuration applies
//! (`--stdin-filename`), and its folder is the working directory.
//!
//! A formatter reads the project's own configuration, and some of those files
//! are programs (`prettier.config.js`): formatting a file runs what its project
//! says, as it would in any editor. Only roots the page may already edit are
//! formatted, and Prettier's search for its configuration — which on its own
//! walks up to `/` — is done here and stops at the root (ED6.5 security
//! review): a configuration above the project is never run. At most
//! [`MAX_RUNNING`] formatters run at once.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::FilesError;
use crate::lsp::servers::{Override, Overrides, find_program};
use crate::root::Root;

/// Stands for the file's absolute path in a formatter's arguments.
pub const FILE_PLACEHOLDER: &str = "{file}";
/// Stands for the Rust edition of the file's crate (`rustfmt`).
const EDITION_PLACEHOLDER: &str = "{edition}";
/// How long one formatter step may take.
pub const FORMAT_TIMEOUT: Duration = Duration::from_secs(10);
/// Most output taken from a formatter (as the editor's largest file).
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// How much of a failing formatter's error output is shown.
const MAX_ERROR_CHARS: usize = 400;
/// Most formatter runs at once; more are refused (a page cannot flood the machine).
pub const MAX_RUNNING: usize = 4;
/// Stands for Prettier's configuration: `--config <file>` found inside the root, or `--no-config`.
const CONFIG_PLACEHOLDER: &str = "{prettier-config}";
/// Prettier's configuration files, in its own order of preference.
const PRETTIER_CONFIGS: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.json5",
    ".prettierrc.js",
    ".prettierrc.mjs",
    ".prettierrc.cjs",
    ".prettierrc.ts",
    ".prettierrc.mts",
    ".prettierrc.cts",
    "prettier.config.js",
    "prettier.config.mjs",
    "prettier.config.cjs",
    "prettier.config.ts",
    "prettier.config.mts",
    "prettier.config.cts",
    ".prettierrc.toml",
];

/// Counts formatter runs against a limit; the app has one ([`RUNNING`]).
struct Limiter {
    running: AtomicUsize,
    limit: usize,
}

static RUNNING: Limiter = Limiter {
    running: AtomicUsize::new(0),
    limit: MAX_RUNNING,
};

/// One run counted while it lives.
struct Slot<'a>(&'a Limiter);

impl Limiter {
    fn take(&self) -> Option<Slot<'_>> {
        self.running
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(self))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A formatter the table knows: the languages it formats and the command lines
/// it runs in turn, each taking the previous one's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterSpec {
    pub id: &'static str,
    pub languages: &'static [&'static str],
    pub steps: &'static [&'static [&'static str]],
}

/// The built-in table (L13), after the owner's conform.nvim configuration.
pub const FORMATTERS: &[FormatterSpec] = &[
    FormatterSpec {
        id: "ruff",
        languages: &["python"],
        steps: &[
            &[
                "ruff",
                "check",
                "--fix",
                "--force-exclude",
                "--exit-zero",
                "--no-cache",
                "--line-length",
                "120",
                "--stdin-filename",
                "{file}",
                "-",
            ],
            &[
                "ruff",
                "check",
                "--fix",
                "--force-exclude",
                "--select=I001",
                "--exit-zero",
                "--no-cache",
                "--line-length",
                "120",
                "--stdin-filename",
                "{file}",
                "-",
            ],
            &[
                "ruff",
                "format",
                "--force-exclude",
                "--line-length",
                "120",
                "--stdin-filename",
                "{file}",
                "-",
            ],
        ],
    },
    FormatterSpec {
        id: "rustfmt",
        languages: &["rust"],
        steps: &[&["rustfmt", "--emit", "stdout", "--edition", "{edition}"]],
    },
    FormatterSpec {
        id: "prettier",
        languages: &[
            "typescript",
            "tsx",
            "javascript",
            "css",
            "html",
            "json",
            "yaml",
        ],
        steps: &[&[
            "prettier",
            "{prettier-config}",
            "--print-width",
            "120",
            "--stdin-filepath",
            "{file}",
        ]],
    },
    // Markdown keeps Prettier's own width: prose is left as written, and a width
    // would only re-pad tables (the owner's conform setup does the same).
    FormatterSpec {
        id: "prettier-markdown",
        languages: &["markdown"],
        steps: &[&[
            "prettier",
            "{prettier-config}",
            "--stdin-filepath",
            "{file}",
        ]],
    },
    FormatterSpec {
        id: "stylua",
        languages: &["lua"],
        steps: &[&[
            "stylua",
            "--search-parent-directories",
            "--stdin-filepath",
            "{file}",
            "-",
        ]],
    },
    FormatterSpec {
        id: "shfmt",
        languages: &["bash"],
        steps: &[&["shfmt", "-filename", "{file}"]],
    },
];

/// What formatting did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Formatted {
    /// The formatted text.
    Done { formatter: String, text: String },
    /// No formatter for this language, or it is turned off: the page asks the language server.
    None,
    /// The formatter is not installed: the page asks the language server, and says so if that has none.
    Missing { formatter: String },
    /// The formatter refused (a syntax error, say): what it said.
    Failed { formatter: String, message: String },
}

/// Formats `text`, the content of `rel` in `root`, with its language's formatter.
///
/// Errors:
///     [`FilesError::Refused`] for a path the root does not allow;
///     [`FilesError::Io`] when a formatter cannot be started.
pub fn format(
    root: &Root,
    rel: &str,
    language: &str,
    text: &str,
    overrides: &Overrides,
    search: &[PathBuf],
) -> Result<Formatted, FilesError> {
    let file = root.resolve(rel)?;
    let Some(spec) = FORMATTERS.iter().find(|f| f.languages.contains(&language)) else {
        return Ok(Formatted::None);
    };
    let steps: Vec<Vec<String>> = match overrides.formatter(spec.id) {
        Some(Override::Enabled(false)) => return Ok(Formatted::None),
        Some(Override::Command { command }) if !command.is_empty() => vec![command.clone()],
        _ => spec
            .steps
            .iter()
            .map(|step| step.iter().map(|a| a.to_string()).collect())
            .collect(),
    };
    let edition = if language == "rust" {
        rust_edition(&file, root.path())
    } else {
        String::new()
    };
    let prettier_config = prettier_config(&file, root.path());
    let Some(_slot) = RUNNING.take() else {
        return Ok(Formatted::Failed {
            formatter: spec.id.to_string(),
            message: format!("{MAX_RUNNING} formatters are running already; try again in a moment"),
        });
    };
    let cwd = file
        .parent()
        .filter(|p| p.is_dir())
        .unwrap_or(root.path())
        .to_path_buf();
    let mut current = text.to_string();
    for step in &steps {
        let (program, args) = step.split_first().expect("steps are never empty");
        let Some(program) = find_program(program, search) else {
            return Ok(Formatted::Missing {
                formatter: spec.id.to_string(),
            });
        };
        let args: Vec<String> = args
            .iter()
            .flat_map(|a| {
                if a == CONFIG_PLACEHOLDER {
                    return prettier_config.clone();
                }
                vec![
                    a.replace(FILE_PLACEHOLDER, &file.to_string_lossy())
                        .replace(EDITION_PLACEHOLDER, &edition),
                ]
            })
            .collect();
        match run_step(&program, &args, &cwd, &current, search)? {
            Ok(out) => current = out,
            Err(message) => {
                return Ok(Formatted::Failed {
                    formatter: spec.id.to_string(),
                    message,
                });
            }
        }
    }
    Ok(Formatted::Done {
        formatter: spec.id.to_string(),
        text: current,
    })
}

/// Runs one step: `input` on stdin, stdout back — or what went wrong, as the
/// inner error (a non-zero exit, output that is too large or not UTF-8, a timeout).
fn run_step(
    program: &Path,
    args: &[String],
    cwd: &Path,
    input: &str,
    search: &[PathBuf],
) -> Result<Result<String, String>, FilesError> {
    let mut command = Command::new(program);
    crate::toolenv::apply(&mut command, search);
    let mut child = command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| FilesError::Io {
            path: program.to_path_buf(),
            source,
        })?;
    // Written and read on threads of their own: a formatter may write before it has read everything.
    let mut stdin = child.stdin.take().expect("piped");
    let input = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut out = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut out);
        // Past the limit the rest is drained, so the formatter can finish instead of blocking on a full pipe.
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        out
    });
    let stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|source| FilesError::Io {
            path: program.to_path_buf(),
            source,
        })? {
            break Some(status);
        }
        if started.elapsed() > FORMAT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    let Some(status) = status else {
        return Ok(Err(format!(
            "took longer than {} s",
            FORMAT_TIMEOUT.as_secs()
        )));
    };
    if !status.success() {
        let message: String = err.trim().chars().take(MAX_ERROR_CHARS).collect();
        return Ok(Err(if message.is_empty() {
            format!("exited with {status}")
        } else {
            message
        }));
    }
    if out.len() > MAX_OUTPUT_BYTES {
        return Ok(Err("its output is larger than the editor takes".into()));
    }
    Ok(String::from_utf8(out).map_err(|_| "its output is not UTF-8".to_string()))
}

/// Prettier's configuration for `file`, looked for from its folder up to
/// `root` and no further: `--config <file>` for the first found (a
/// `package.json` with a `"prettier"` key counts), else `--no-config`.
fn prettier_config(file: &Path, root: &Path) -> Vec<String> {
    for dir in file.ancestors().skip(1) {
        if !dir.starts_with(root) {
            break;
        }
        for name in PRETTIER_CONFIGS {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return vec!["--config".into(), candidate.to_string_lossy().into_owned()];
            }
        }
        let package = dir.join("package.json");
        let has_key = std::fs::read_to_string(&package)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .is_some_and(|json| json.get("prettier").is_some());
        if has_key {
            return vec!["--config".into(), package.to_string_lossy().into_owned()];
        }
    }
    vec!["--no-config".into()]
}

/// The edition of the crate `file` belongs to: the nearest `Cargo.toml` above
/// it (up to `root`) that states one — a crate inheriting it from its workspace
/// is looked past. `2021` when none says.
fn rust_edition(file: &Path, root: &Path) -> String {
    let edition = regex::Regex::new(r#"(?m)^\s*edition\s*=\s*"(\d{4})""#).expect("valid pattern");
    for dir in file.ancestors().skip(1) {
        // Nothing above the root is read.
        if !dir.starts_with(root) {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml"))
            && let Some(found) = edition.captures(&text)
        {
            return found[1].to_string();
        }
        if dir == root {
            break;
        }
    }
    "2021".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root::LinkPolicy;
    use std::os::unix::fs::PermissionsExt;

    fn setup(name: &str) -> (PathBuf, Root, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("axiomata-format-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project/src")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let root = Root::dir(&dir.join("project"), LinkPolicy::Contained).unwrap();
        (dir.clone(), root, dir.join("bin"))
    }

    fn program(bin: &Path, name: &str, script: &str) {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn runs_every_step_on_the_previous_output_with_the_file_named() {
        let (dir, root, bin) = setup("steps");
        // Each ruff step appends its first argument and the file it was told.
        program(
            &bin,
            "ruff",
            r#"cat; printf '%s %s\n' "$1" "$(echo "$@" | tr ' ' '\n' | grep '/a.py')""#,
        );
        let out = format(
            &root,
            "src/a.py",
            "python",
            "x\n",
            &Overrides::default(),
            &[bin],
        )
        .unwrap();
        let Formatted::Done { formatter, text } = out else {
            panic!("{out:?}")
        };
        assert_eq!(formatter, "ruff");
        let file = root.path().join("src/a.py");
        let line = format!("{}\n", file.display());
        assert_eq!(text, format!("x\ncheck {line}check {line}format {line}"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reports_a_missing_or_failing_formatter_and_knows_languages_without_one() {
        let (dir, root, bin) = setup("fail");
        let none = format(
            &root,
            "a.toml",
            "toml",
            "",
            &Overrides::default(),
            std::slice::from_ref(&bin),
        )
        .unwrap();
        assert_eq!(none, Formatted::None);
        let missing = format(
            &root,
            "a.lua",
            "lua",
            "",
            &Overrides::default(),
            std::slice::from_ref(&bin),
        )
        .unwrap();
        assert_eq!(
            missing,
            Formatted::Missing {
                formatter: "stylua".into()
            }
        );
        program(&bin, "shfmt", "echo 'line 1: syntax error' >&2; exit 1");
        let failed = format(&root, "a.sh", "bash", "if", &Overrides::default(), &[bin]).unwrap();
        assert_eq!(
            failed,
            Formatted::Failed {
                formatter: "shfmt".into(),
                message: "line 1: syntax error".into()
            }
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn honours_the_override_file_and_refuses_a_path_outside_the_root() {
        let (dir, root, bin) = setup("override");
        program(&bin, "myfmt", "tr a-z A-Z");
        let overrides: Overrides = serde_json::from_str(
            r#"{"formatters": {"shfmt": {"command": ["myfmt"]}, "prettier": false}}"#,
        )
        .unwrap();
        let out = format(
            &root,
            "a.sh",
            "bash",
            "echo",
            &overrides,
            std::slice::from_ref(&bin),
        )
        .unwrap();
        assert_eq!(
            out,
            Formatted::Done {
                formatter: "shfmt".into(),
                text: "ECHO".into()
            }
        );
        let off = format(
            &root,
            "a.ts",
            "typescript",
            "x",
            &overrides,
            std::slice::from_ref(&bin),
        )
        .unwrap();
        assert_eq!(off, Formatted::None);
        assert!(format(&root, "../x.sh", "bash", "", &overrides, &[bin]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn looks_for_prettiers_configuration_only_inside_the_root() {
        let (dir, root, _) = setup("prettierrc");
        let project = root.path();
        // Above the root: never used.
        std::fs::write(dir.join("prettier.config.js"), "module.exports = {}").unwrap();
        assert_eq!(
            prettier_config(&project.join("src/a.ts"), project),
            vec!["--no-config"]
        );
        std::fs::write(
            project.join("package.json"),
            r#"{"prettier": {"semi": false}}"#,
        )
        .unwrap();
        let found = prettier_config(&project.join("src/a.ts"), project);
        assert_eq!(
            found,
            vec![
                "--config".to_string(),
                project.join("package.json").display().to_string()
            ]
        );
        std::fs::write(project.join("src/.prettierrc"), "{}").unwrap();
        let nearer = prettier_config(&project.join("src/a.ts"), project);
        assert_eq!(
            nearer[1],
            project.join("src/.prettierrc").display().to_string()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_when_too_many_formatters_run() {
        let limiter = Limiter {
            running: AtomicUsize::new(0),
            limit: 2,
        };
        let held: Vec<Slot> = std::iter::from_fn(|| limiter.take()).take(2).collect();
        assert_eq!(held.len(), 2);
        assert!(limiter.take().is_none());
        drop(held);
        assert!(limiter.take().is_some());
    }

    #[test]
    fn finds_the_crates_edition_past_an_inherited_one() {
        let (dir, root, _) = setup("edition");
        let project = root.path();
        std::fs::write(
            project.join("Cargo.toml"),
            "[workspace.package]\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.join("crates/a/src")).unwrap();
        std::fs::write(
            project.join("crates/a/Cargo.toml"),
            "[package]\nedition.workspace = true\n",
        )
        .unwrap();
        assert_eq!(
            rust_edition(&project.join("crates/a/src/lib.rs"), project),
            "2024"
        );
        assert_eq!(
            rust_edition(&project.join("x.rs"), &project.join("crates")),
            "2021"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
