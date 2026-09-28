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

/// Sends a request and waits for its answer, answering the server's own requests meanwhile.
fn ask(
    host: &LspHost,
    handle: u64,
    rx: &mpsc::Receiver<String>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    host.send(
        handle,
        "live",
        &serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        let Ok(message) = rx.recv_timeout(Duration::from_secs(1)) else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&message).unwrap();
        if value.get("method").is_some() && value.get("id").is_some() {
            host.send(
                handle,
                "live",
                &serde_json::json!({"jsonrpc":"2.0","id":value["id"],"result":null}).to_string(),
            )
            .unwrap();
            continue;
        }
        if value["id"] == id {
            return value;
        }
    }
    panic!("no answer to {method}");
}

/// Starts rust-analyzer on the crate in `dir`, initialized, with `src/main.rs` open.
fn open_rust(dir: &std::path::Path) -> (LspHost, u64, mpsc::Receiver<String>, String) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let host = LspHost::new(
        dir.join("no-lsp.json"),
        search_path(std::env::var_os("PATH").as_deref(), home.as_deref()),
    );
    let root = Root::dir(dir, LinkPolicy::Contained).unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    let Started::Running { handle, .. } = host
        .start(
            "project:live",
            &root,
            "rust",
            "live",
            Arc::new(move |m| drop(tx.lock().unwrap().send(m))),
        )
        .unwrap()
    else {
        panic!("rust-analyzer not running")
    };
    let root_uri = uri(dir);
    ask(
        &host,
        handle,
        &rx,
        1,
        "initialize",
        serde_json::json!({
            "processId": null, "rootUri": root_uri, "workspaceFolders": [{"uri": root_uri, "name": "live"}],
            "capabilities": {"general": {"positionEncodings": ["utf-16"]},
                "textDocument": {"completion": {"completionItem": {
                    "snippetSupport": true,
                    "resolveSupport": {"properties": ["documentation", "detail", "additionalTextEdits"]}
                }}}}
        }),
    );
    host.send(
        handle,
        "live",
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
    )
    .unwrap();
    let file = dir.join("src/main.rs");
    let file_uri = uri(&file);
    host.send(
        handle,
        "live",
        &serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
        "textDocument": {"uri": file_uri, "languageId": "rust", "version": 1,
                         "text": std::fs::read_to_string(&file).unwrap()}}})
        .to_string(),
    )
    .unwrap();

    (host, handle, rx, file_uri)
}

#[test]
#[ignore = "starts the real rust-analyzer"]
fn rust_analyzer_completes_methods_and_an_auto_import() {
    let dir = scratch(
        "rust-complete",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/main.rs",
                "fn main() {\n    let v: Vec<u8> = Vec::new();\n    v.\n    let m = HashMa\n}\n",
            ),
        ],
    );
    let (host, handle, rx, file_uri) = open_rust(&dir);
    let at = |line: u32, character: u32, trigger: Option<&str>| {
        let context = match trigger {
            Some(c) => serde_json::json!({"triggerKind": 2, "triggerCharacter": c}),
            None => serde_json::json!({"triggerKind": 1}),
        };
        serde_json::json!({"textDocument": {"uri": file_uri},
            "position": {"line": line, "character": character}, "context": context})
    };
    let labels = |answer: &serde_json::Value| -> Vec<String> {
        let items = answer["result"]["items"]
            .as_array()
            .or(answer["result"].as_array())
            .cloned()
            .unwrap_or_default();
        items
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };
    // rust-analyzer answers once it has loaded the crate; ask until it knows `Vec`.
    let mut methods = serde_json::Value::Null;
    for id in 10..70 {
        methods = ask(
            &host,
            handle,
            &rx,
            id,
            "textDocument/completion",
            at(2, 6, Some(".")),
        );
        if labels(&methods).iter().any(|l| l.starts_with("push")) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        labels(&methods).iter().any(|l| l.starts_with("push")),
        "{:?}",
        labels(&methods)
    );

    let names = ask(
        &host,
        handle,
        &rx,
        80,
        "textDocument/completion",
        at(3, 18, None),
    );
    let items = names["result"]["items"]
        .as_array()
        .or(names["result"].as_array())
        .cloned()
        .unwrap_or_default();
    let hash_map = items
        .iter()
        .find(|i| {
            i["label"]
                .as_str()
                .is_some_and(|l| l.starts_with("HashMap"))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no HashMap in {:?}", labels(&names)));
    let resolved = ask(&host, handle, &rx, 81, "completionItem/resolve", hash_map);
    let imports = resolved["result"]["additionalTextEdits"].to_string();
    println!("HashMap resolves to {imports}");
    assert!(
        imports.contains("use std::collections::HashMap"),
        "{resolved}"
    );
    host.stop(handle);
}

#[test]
#[ignore = "starts the real rust-analyzer"]
fn rust_analyzer_hovers_and_its_std_definition_becomes_readable() {
    let dir = scratch(
        "rust-def",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/main.rs",
                concat!(
                    "fn helper() {}\nfn main() {\n    helper();\n    let s = String::new();\n}\n",
                    "trait Speak { fn speak(&self); }\nstruct Dog;\nimpl Speak for Dog { fn speak(&self) {} }\n",
                ),
            ),
        ],
    );
    let (host, handle, rx, file_uri) = open_rust(&dir);

    let at = |line: u32, character: u32| {
        let position = serde_json::json!({"line": line, "character": character});
        serde_json::json!({"textDocument": {"uri": file_uri}, "position": position})
    };
    // rust-analyzer answers once it has loaded the crate; ask until it knows.
    let mut hover = serde_json::Value::Null;
    for id in 10..40 {
        hover = ask(&host, handle, &rx, id, "textDocument/hover", at(2, 5));
        if !hover["result"].is_null() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("hover: {}", hover["result"]["contents"]);
    assert!(
        hover["result"]["contents"]
            .to_string()
            .contains("fn helper"),
        "{hover}"
    );

    let local = ask(&host, handle, &rx, 50, "textDocument/definition", at(2, 5));
    assert!(local["result"].to_string().contains("main.rs"), "{local}");

    // ED6.3: the uses of `helper` (its declaration too), the implementations of `Speak`.
    let mut uses = at(2, 5);
    uses["context"] = serde_json::json!({"includeDeclaration": true});
    let uses = ask(&host, handle, &rx, 52, "textDocument/references", uses);
    assert_eq!(uses["result"].as_array().map(Vec::len), Some(2), "{uses}");
    let impls = ask(
        &host,
        handle,
        &rx,
        53,
        "textDocument/implementation",
        at(5, 7),
    );
    println!("Speak implemented at {}", impls["result"]);
    let impl_line = impls["result"][0]["range"]["start"]["line"]
        .as_u64()
        .or(impls["result"][0]["targetSelectionRange"]["start"]["line"].as_u64());
    assert_eq!(impl_line, Some(7), "{impls}");

    let std_def = ask(&host, handle, &rx, 51, "textDocument/definition", at(3, 13));
    println!("String -> {}", std_def["result"]);
    let target = std_def["result"][0]["targetUri"]
        .as_str()
        .or(std_def["result"][0]["uri"].as_str());
    let Some(target) = target else {
        eprintln!(
            "no std sources installed (rustup component add rust-src) — the foreign read is not checked"
        );
        return;
    };
    let path = axiomata_files::lsp::file_uri_path(target).unwrap();
    assert!(!path.starts_with(&dir), "String lives outside the project");
    let read = host
        .read_foreign(handle, &path, 16 * 1024 * 1024)
        .expect("the server named it");
    assert!(
        read.content.contains("pub struct String"),
        "{}",
        path.display()
    );
    host.stop(handle);
}
