//! Which language server runs for which language (editor plan L2, L10).
//!
//! The decision is Rust's alone: a built-in table of well-known servers, each
//! found on `PATH` (plus the usual install places, because an app started from
//! the Finder does not inherit a shell's `PATH`), and overrides from a file
//! only Rust reads — `~/.axiomata/lsp.json`, which no command writes. The
//! editor's own settings, which the webview does write, never name a program
//! to start.
//!
//! One server can serve several languages (TypeScript, JavaScript and TSX
//! share one); a server is identified by its own id, and the host runs one per
//! (root, server).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A server the table knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// The server's id — what the host keys running servers by.
    pub id: &'static str,
    /// Editor language ids it serves.
    pub languages: &'static [&'static str],
    /// Candidate command lines, first found wins.
    pub commands: &'static [&'static [&'static str]],
    /// How to install it, for the quiet hint when none is found.
    pub install: &'static str,
}

/// The built-in table: the editor's tree-sitter languages that have a
/// well-known server (L10).
pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        id: "rust-analyzer",
        languages: &["rust"],
        commands: &[&["rust-analyzer"]],
        install: "rustup component add rust-analyzer",
    },
    ServerSpec {
        id: "pyright",
        languages: &["python"],
        commands: &[
            &["basedpyright-langserver", "--stdio"],
            &["pyright-langserver", "--stdio"],
            &["pylsp"],
        ],
        install: "npm i -g pyright",
    },
    ServerSpec {
        id: "typescript",
        languages: &["typescript", "javascript", "tsx"],
        commands: &[
            &["typescript-language-server", "--stdio"],
            &["vtsls", "--stdio"],
        ],
        install: "npm i -g typescript typescript-language-server",
    },
    ServerSpec {
        id: "svelte",
        languages: &["svelte"],
        commands: &[&["svelteserver", "--stdio"]],
        install: "npm i -g svelte-language-server",
    },
    ServerSpec {
        id: "css",
        languages: &["css"],
        commands: &[&["vscode-css-language-server", "--stdio"]],
        install: "npm i -g vscode-langservers-extracted",
    },
    ServerSpec {
        id: "html",
        languages: &["html"],
        commands: &[&["vscode-html-language-server", "--stdio"]],
        install: "npm i -g vscode-langservers-extracted",
    },
    ServerSpec {
        id: "json",
        languages: &["json"],
        commands: &[&["vscode-json-language-server", "--stdio"]],
        install: "npm i -g vscode-langservers-extracted",
    },
    ServerSpec {
        id: "bash",
        languages: &["bash"],
        commands: &[&["bash-language-server", "start"]],
        install: "npm i -g bash-language-server",
    },
    ServerSpec {
        id: "lua",
        languages: &["lua"],
        commands: &[&["lua-language-server"]],
        install: "brew install lua-language-server",
    },
    ServerSpec {
        id: "sourcekit",
        languages: &["swift"],
        commands: &[&["sourcekit-lsp"]],
        install: "xcode-select --install",
    },
    ServerSpec {
        id: "taplo",
        languages: &["toml"],
        commands: &[&["taplo", "lsp", "stdio"]],
        install: "brew install taplo",
    },
    ServerSpec {
        id: "yaml",
        languages: &["yaml"],
        commands: &[&["yaml-language-server", "--stdio"]],
        install: "npm i -g yaml-language-server",
    },
    ServerSpec {
        id: "marksman",
        languages: &["markdown"],
        commands: &[&["marksman", "server"]],
        install: "brew install marksman",
    },
    ServerSpec {
        id: "sqls",
        languages: &["sql"],
        commands: &[&["sqls"]],
        install: "go install github.com/sqls-server/sqls@latest",
    },
];

/// The server the table names for an editor language.
pub fn spec_for(language: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.languages.contains(&language))
}

/// What `~/.axiomata/lsp.json` may say about one server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Override {
    /// `false` turns the server off.
    Enabled(bool),
    /// `{"command": ["bin", "arg", …]}` replaces the table's command lines.
    Command { command: Vec<String> },
}

/// The override file: `{"servers": {"<server id>": false | {"command": [...]}},
/// "formatters": {"<formatter id>": false | {"command": [...]}}}` (a formatter's
/// command may name the file as `{file}`, `crate::format`).
#[derive(Debug, Default, Deserialize)]
pub struct Overrides {
    #[serde(default)]
    servers: std::collections::HashMap<String, Override>,
    #[serde(default)]
    formatters: std::collections::HashMap<String, Override>,
}

impl Overrides {
    /// Reads the file; missing means none, unreadable is an error string for the hint.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    fn get(&self, server: &str) -> Option<&Override> {
        self.servers.get(server)
    }

    /// What the file says about one formatter (editor plan L13).
    pub fn formatter(&self, id: &str) -> Option<&Override> {
        self.formatters.get(id)
    }
}

/// How a language's server is started, or why it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Found: the server's id, the program's absolute path, and its arguments.
    Found {
        server: &'static str,
        program: PathBuf,
        args: Vec<String>,
    },
    /// The table has no server for this language.
    NoServer,
    /// Turned off in the override file.
    Disabled { server: &'static str },
    /// Not installed (or the override's program is not there): how to get it.
    Missing {
        server: &'static str,
        install: String,
    },
}

/// Resolves `language` against the table, the overrides and the file system.
///
/// `search` is where programs are looked for, in order (see [`search_path`]).
pub fn resolve(language: &str, overrides: &Overrides, search: &[PathBuf]) -> Resolution {
    let Some(spec) = spec_for(language) else {
        return Resolution::NoServer;
    };
    match overrides.get(spec.id) {
        Some(Override::Enabled(false)) => return Resolution::Disabled { server: spec.id },
        Some(Override::Command { command }) => {
            let Some((program, args)) = command.split_first() else {
                return Resolution::Missing {
                    server: spec.id,
                    install: "an empty \"command\" in ~/.axiomata/lsp.json".into(),
                };
            };
            return match find_program(program, search) {
                Some(program) => Resolution::Found {
                    server: spec.id,
                    program,
                    args: args.to_vec(),
                },
                None => Resolution::Missing {
                    server: spec.id,
                    install: format!("{program} (from ~/.axiomata/lsp.json) was not found"),
                },
            };
        }
        Some(Override::Enabled(true)) | None => {}
    }
    for candidate in spec.commands {
        let (program, args) = candidate
            .split_first()
            .expect("table commands are never empty");
        if let Some(found) = find_program(program, search) {
            return Resolution::Found {
                server: spec.id,
                program: found,
                args: args.iter().map(|a| a.to_string()).collect(),
            };
        }
    }
    Resolution::Missing {
        server: spec.id,
        install: spec.install.to_string(),
    }
}

/// How long asking a toolchain where it lives (`rustc --print sysroot`) may take.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The folders outside every root whose files a server may point to as
/// definitions (editor plan L11, narrowed by the ED6.2 security review): the
/// server's own install (a Homebrew keg, a global `node_modules`) and its
/// language's toolchain — Rust's sysroot and `~/.cargo` sources, Python's
/// prefix, Xcode. Worked out here from the machine, never from what a server
/// says: a project file can make a server name any path (`#[path = …]`), so a
/// named path outside these folders is never made readable. Canonical paths.
pub fn toolchain_roots(
    server: &str,
    program: &Path,
    search: &[PathBuf],
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = install_roots(program);
    match server {
        "rust-analyzer" => {
            roots.extend(ask(search, "rustc", &["--print", "sysroot"]));
            if let Some(home) = home {
                for dir in [
                    ".cargo/registry/src",
                    ".cargo/git/checkouts",
                    ".rustup/toolchains",
                ] {
                    roots.push(home.join(dir));
                }
            }
        }
        "pyright" => {
            roots.extend(ask(
                search,
                "python3",
                &[
                    "-c",
                    "import sys; print(sys.base_prefix); print(sys.prefix)",
                ],
            ));
        }
        "sourcekit" => {
            roots.push(PathBuf::from("/Applications/Xcode.app"));
            roots.push(PathBuf::from("/Library/Developer/CommandLineTools"));
        }
        _ => {}
    }
    let home = home.and_then(|h| h.canonicalize().ok());
    let mut canonical: Vec<PathBuf> = roots
        .into_iter()
        .filter_map(|r| r.canonicalize().ok())
        // A toolchain that reports `/`, the home folder or anything above it (a
        // Python prefix of `$HOME`) would free every file there: such a root is dropped.
        .filter(|r| r.parent().is_some() && !home.as_ref().is_some_and(|h| h.starts_with(r)))
        .collect();
    canonical.sort();
    canonical.dedup();
    canonical
}

/// The server's own install: the Homebrew keg (`…/Cellar/<name>`) or the
/// global `node_modules` its program lives in (TypeScript's `lib.*.d.ts`,
/// pyright's bundled typeshed).
fn install_roots(program: &Path) -> Vec<PathBuf> {
    let Ok(real) = program.canonicalize() else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for ancestor in real.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == "node_modules") {
            roots.push(ancestor.to_path_buf());
            break;
        }
        if ancestor
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "Cellar")
        {
            roots.push(ancestor.to_path_buf());
            break;
        }
    }
    roots
}

/// The lines a toolchain program prints, as paths (nothing if it is missing,
/// fails, or takes longer than [`ASK_TIMEOUT`]).
fn ask(search: &[PathBuf], program: &str, args: &[&str]) -> Vec<PathBuf> {
    let Some(program) = find_program(program, search) else {
        return Vec::new();
    };
    let Ok(mut child) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = std::time::Instant::now() + ASK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
    }
    out.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

/// Where programs are looked for: `PATH`, then the usual install places a
/// Finder-started app would otherwise miss.
pub fn search_path(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let mut extra = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        extra.push(home.join(".cargo/bin"));
        extra.push(home.join(".local/bin"));
        // The owner's Neovim tools (editor plan L15), last: prettier, stylua, language servers.
        extra.push(home.join(".local/share/nvim/mason/bin"));
    }
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// An absolute program path as given, or the first executable of that name in `search`.
pub(crate) fn find_program(program: &str, search: &[PathBuf]) -> Option<PathBuf> {
    let direct = Path::new(program);
    if direct.is_absolute() {
        return is_executable(direct).then(|| direct.to_path_buf());
    }
    if program.contains('/') {
        // Relative paths would depend on the working directory; not accepted.
        return None;
    }
    search
        .iter()
        .map(|dir| dir.join(program))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn bin_dir(name: &str, programs: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("axiomata-lsp-bin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for program in programs {
            let file = dir.join(program);
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn every_table_language_maps_to_one_server_and_typescript_is_shared() {
        assert_eq!(spec_for("tsx").unwrap().id, "typescript");
        assert_eq!(spec_for("javascript").unwrap().id, "typescript");
        assert_eq!(spec_for("rust").unwrap().id, "rust-analyzer");
        assert!(spec_for("markdown_inline").is_none());
        let mut seen = std::collections::HashSet::new();
        for spec in SERVERS {
            for language in spec.languages {
                assert!(seen.insert(*language), "{language} is served twice");
            }
            assert!(!spec.commands.is_empty() && spec.commands.iter().all(|c| !c.is_empty()));
        }
    }

    #[test]
    fn the_first_installed_candidate_wins() {
        let dir = bin_dir("first", &["pyright-langserver", "pylsp"]);
        let found = resolve("python", &Overrides::default(), std::slice::from_ref(&dir));
        assert_eq!(
            found,
            Resolution::Found {
                server: "pyright",
                program: dir.join("pyright-langserver"),
                args: vec!["--stdio".into()]
            }
        );
        assert_eq!(
            resolve("lua", &Overrides::default(), std::slice::from_ref(&dir)),
            Resolution::Missing {
                server: "lua",
                install: "brew install lua-language-server".into()
            }
        );
        assert_eq!(
            resolve("cobol", &Overrides::default(), &[dir]),
            Resolution::NoServer
        );
    }

    #[test]
    fn overrides_replace_or_turn_off_a_server() {
        let dir = bin_dir("override", &["my-ra", "rust-analyzer"]);
        let overrides: Overrides = serde_json::from_str(
            r#"{"servers": {"rust-analyzer": {"command": ["my-ra", "--log"]}, "typescript": false}}"#,
        )
        .unwrap();
        assert_eq!(
            resolve("rust", &overrides, std::slice::from_ref(&dir)),
            Resolution::Found {
                server: "rust-analyzer",
                program: dir.join("my-ra"),
                args: vec!["--log".into()]
            }
        );
        assert_eq!(
            resolve("tsx", &overrides, std::slice::from_ref(&dir)),
            Resolution::Disabled {
                server: "typescript"
            }
        );
        let relative: Overrides =
            serde_json::from_str(r#"{"servers": {"rust-analyzer": {"command": ["./bin/ra"]}}}"#)
                .unwrap();
        assert!(matches!(
            resolve("rust", &relative, &[dir]),
            Resolution::Missing { .. }
        ));
    }

    #[test]
    fn a_missing_override_file_is_no_override_and_a_broken_one_says_so() {
        let dir = bin_dir("file", &[]);
        assert!(
            Overrides::load(&dir.join("lsp.json"))
                .unwrap()
                .servers
                .is_empty()
        );
        std::fs::write(dir.join("lsp.json"), "{ nope").unwrap();
        assert!(
            Overrides::load(&dir.join("lsp.json"))
                .unwrap_err()
                .contains("lsp.json")
        );
    }

    #[test]
    fn a_toolchain_root_at_or_above_home_is_dropped() {
        let dir = std::env::temp_dir().join(format!("ax-lsp-home-{}", std::process::id()));
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".cargo/registry/src")).unwrap();
        let program = home.join("bin/rust-analyzer");
        let roots = toolchain_roots("rust-analyzer", &program, &[], Some(&home));
        let home = home.canonicalize().unwrap();
        assert!(roots.contains(&home.join(".cargo/registry/src")));
        assert!(roots.iter().all(|r| !home.starts_with(r)), "{roots:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_servers_install_root_is_its_keg_or_its_node_modules() {
        let base =
            std::env::temp_dir().join(format!("axiomata-lsp-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let keg = base.join("Cellar/pyright/1.1/libexec/lib/node_modules/pyright/index.js");
        let brew = base.join("Cellar/rust-analyzer/2026/bin/rust-analyzer");
        for file in [&keg, &brew] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        let base = base.canonicalize().unwrap();
        assert_eq!(
            install_roots(&keg),
            vec![base.join("Cellar/pyright/1.1/libexec/lib/node_modules")]
        );
        assert_eq!(
            install_roots(&brew),
            vec![base.join("Cellar/rust-analyzer")]
        );
        assert!(install_roots(Path::new("/nonexistent/x")).is_empty());
    }

    #[test]
    fn the_search_path_adds_the_usual_install_places_once() {
        let dirs = search_path(
            Some(std::ffi::OsStr::new("/usr/bin:/opt/homebrew/bin")),
            Some(Path::new("/h")),
        );
        assert_eq!(
            dirs,
            [
                "/usr/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/h/.cargo/bin",
                "/h/.local/bin",
                "/h/.local/share/nvim/mason/bin",
            ]
            .map(PathBuf::from)
            .to_vec()
        );
    }
}
