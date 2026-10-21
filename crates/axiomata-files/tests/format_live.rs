//! Live checks against the real formatters — `#[ignore]`d, run on demand:
//! `cargo test -p axiomata-files --test format_live -- --ignored`.
//! Each check is skipped (with a note) when its formatter is not installed.

use std::path::PathBuf;

use axiomata_files::format::{Formatted, format};
use axiomata_files::lsp::servers::{Overrides, search_path};
use axiomata_files::root::{LinkPolicy, Root};

fn scratch(name: &str, files: &[(&str, &str)]) -> Root {
    let dir = std::env::temp_dir().join(format!(
        "axiomata-format-live-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    Root::dir(&dir, LinkPolicy::Contained).unwrap()
}

fn search() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    search_path(std::env::var_os("PATH").as_deref(), home.as_deref())
}

fn run(root: &Root, rel: &str, language: &str, text: &str) -> Option<String> {
    match format(root, rel, language, text, &Overrides::default(), &search()).unwrap() {
        Formatted::Done { text, .. } => Some(text),
        Formatted::Missing { formatter } => {
            eprintln!("{formatter} not installed — skipped");
            None
        }
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "runs the real ruff"]
fn ruff_sorts_imports_and_formats() {
    let root = scratch("py", &[]);
    let Some(out) = run(
        &root,
        "a.py",
        "python",
        "import sys\nimport os\nx=os.sep+sys.platform\n",
    ) else {
        return;
    };
    assert_eq!(out, "import os\nimport sys\n\nx = os.sep + sys.platform\n");
}

#[test]
#[ignore = "runs the real rustfmt"]
fn rustfmt_formats_with_the_crates_edition() {
    let root = scratch(
        "rs",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nedition = \"2024\"\n",
            ),
            ("src/main.rs", ""),
        ],
    );
    let Some(out) = run(
        &root,
        "src/main.rs",
        "rust",
        "fn main(){let x=1;if let Some(y)=Some(x)&&y>0{}}",
    ) else {
        return;
    };
    assert!(out.starts_with("fn main() {\n    let x = 1;\n"), "{out}");
}

#[test]
#[ignore = "runs the real prettier"]
fn prettier_formats_typescript() {
    let root = scratch("ts", &[]);
    let Some(out) = run(&root, "a.ts", "typescript", "const a={b:1,c:[1,2]}") else {
        return;
    };
    assert_eq!(out, "const a = { b: 1, c: [1, 2] };\n");
}

#[test]
#[ignore = "runs the real ruff"]
fn a_syntax_error_is_reported_not_applied() {
    let root = scratch("bad", &[]);
    let out = format(
        &root,
        "a.py",
        "python",
        "def (:\n",
        &Overrides::default(),
        &search(),
    )
    .unwrap();
    match out {
        Formatted::Failed { formatter, message } => {
            println!("{formatter}: {message}");
            assert_eq!(formatter, "ruff");
        }
        Formatted::Missing { .. } => {}
        other => panic!("{other:?}"),
    }
}
