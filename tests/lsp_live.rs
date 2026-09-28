//! Live checks against real language servers — `#[ignore]`d, run on demand:
//!
//! ```sh
//! cargo test -p axiomata-files --test lsp_live -- --ignored --nocapture
//! ```
//!
//! Each builds a scratch project with one error in it, starts the server
//! through the host exactly as the app does, and waits for the diagnostic.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axiomata_files::lsp::{LspHost, Started, servers::search_path};
use axiomata_files::{LinkPolicy, Root};

fn scratch(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("axiomata-lsp-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir.canonicalize().unwrap()
}

fn uri(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

/// Starts `language`'s server in `dir`, opens `rel`, and returns the first
/// non-empty `publishDiagnostics` for it.
fn first_diagnostics(
    dir: &std::path::Path,
    language: &str,
    language_id: &str,
    rel: &str,
) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let host = LspHost::new(
        dir.join("no-lsp.json"),
        search_path(std::env::var_os("PATH").as_deref(), home.as_deref()),
    );
    let root = Root::dir(dir, LinkPolicy::Contained).unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    let started = host
        .start(
            "project:live",
            &root,
            language,
            "live",
            Arc::new(move |m| drop(tx.lock().unwrap().send(m))),
        )
        .unwrap();
    let Started::Running { handle, .. } = started else {
        panic!("{language} server not running: {started:?}")
    };
    let root_uri = uri(dir);
    host.send(handle, "live",
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "processId": null, "rootUri": root_uri, "workspaceFolders": [{"uri": root_uri, "name": "live"}],
            "capabilities": {"textDocument": {"publishDiagnostics": {}}, "general": {"positionEncodings": ["utf-16"]}}
        }})
        .to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let file = dir.join(rel);
    let file_uri = uri(&file);
    let mut initialized = false;
    while Instant::now() < deadline {
        let Ok(message) = rx.recv_timeout(Duration::from_secs(1)) else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&message).unwrap();
        if !initialized && value["id"] == 1 {
            initialized = true;
            host.send(
                handle,
                "live",
                r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            )
            .unwrap();
            host.send(handle, "live",
                &serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{
                    "uri": file_uri, "languageId": language_id, "version": 1,
                    "text": std::fs::read_to_string(&file).unwrap()
                }}})
                .to_string(),
            )
            .unwrap();
            continue;
        }
        // Servers also ask things (configuration, progress tokens); answer with null.
        if value.get("method").is_some() && value.get("id").is_some() {
            let answer = if value["method"] == "workspace/configuration" {
                serde_json::json!(vec![
                    serde_json::Value::Null;
                    value["params"]["items"].as_array().map_or(0, Vec::len)
                ])
            } else {
                serde_json::Value::Null
            };
            host.send(
                handle,
                "live",
                &serde_json::json!({"jsonrpc":"2.0","id":value["id"],"result":answer}).to_string(),
            )
            .unwrap();
            continue;
        }
        if value["method"] == "textDocument/publishDiagnostics"
            && value["params"]["uri"] == file_uri
            && value["params"]["diagnostics"]
                .as_array()
                .is_some_and(|d| !d.is_empty())
        {
            host.stop(handle);
            return value["params"]["diagnostics"].to_string();
        }
    }
    host.stop(handle);
    panic!("no diagnostics from the {language} server within the time limit");
}

#[test]
#[ignore = "starts the real rust-analyzer"]
fn rust_analyzer_reports_a_type_error() {
    let dir = scratch(
        "rust",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/main.rs",
                "fn main() {\n    let x: u32 = \"text\";\n}\n",
            ),
        ],
    );
    let diagnostics = first_diagnostics(&dir, "rust", "rust", "src/main.rs");
    println!("{diagnostics}");
    assert!(diagnostics.contains("\"line\":1"), "{diagnostics}");
}

#[test]
#[ignore = "starts the real pyright"]
fn pyright_reports_an_undefined_name() {
    let dir = scratch("python", &[("main.py", "print(undefined_name)\n")]);
    let diagnostics = first_diagnostics(&dir, "python", "python", "main.py");
    println!("{diagnostics}");
    assert!(diagnostics.contains("undefined_name"), "{diagnostics}");
}
