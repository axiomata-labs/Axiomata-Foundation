//! The environment the editor's tools run in — language servers
//! ([`crate::lsp`]), formatters ([`crate::format`]) and the toolchain
//! questions asked about them (`rustc --print sysroot`).
//!
//! They get an allow-listed subset of the app's environment, never all of it:
//! an app started from a shell carries whatever that shell exported, API keys
//! included, and a project's own configuration can make a tool run code
//! (`prettier.config.js`). What they keep is what finding and running a
//! toolchain needs — home, user, locale, temp folder, and the toolchains' own
//! variables (`CARGO_HOME`, `VIRTUAL_ENV`, …). `NODE_OPTIONS` is deliberately
//! not among them: it loads code into every Node program. `PATH` is the
//! search path the editor itself looks for programs on, so a server's
//! `#!/usr/bin/env node` finds the same `node` the editor would.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Variables kept by exact name.
const KEPT: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TMPDIR",
    "TERM",
    // Rust
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    // Python
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "PYENV_ROOT",
    "PYENV_VERSION",
    // Node
    "NVM_DIR",
    "NODE_PATH",
    // Swift and the Apple toolchains
    "DEVELOPER_DIR",
    "SDKROOT",
    "TOOLCHAINS",
    // Others the tables may grow into
    "JAVA_HOME",
    "GOPATH",
    "GOROOT",
];

/// Always at the end of `PATH`.
const SYSTEM_PATH: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Prefixes whose every variable is kept: locale categories, the XDG base
/// folders, OpenSSL's trust store, and what macOS's CoreFoundation injects.
const KEPT_PREFIXES: &[&str] = &["LC_", "XDG_", "SSL_CERT_", "__CF"];

/// The environment for a tool: the kept part of `ambient`, and `PATH` made of
/// `search` followed by the system's own folders.
pub fn tool_env_from<I>(ambient: I, search: &[PathBuf]) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut env: Vec<(OsString, OsString)> = ambient
        .into_iter()
        .filter(|(key, _)| {
            key.to_str().is_some_and(|k| {
                k != "PATH" && (KEPT.contains(&k) || KEPT_PREFIXES.iter().any(|p| k.starts_with(p)))
            })
        })
        .collect();
    // The system's own folders last: a script's `cat`, `sh` or `env` must be found.
    let mut path: Vec<PathBuf> = search.to_vec();
    for dir in SYSTEM_PATH {
        if !path.iter().any(|p| p == Path::new(dir)) {
            path.push(PathBuf::from(dir));
        }
    }
    if let Ok(joined) = std::env::join_paths(path) {
        env.push(("PATH".into(), joined));
    }
    env
}

/// Gives `command` the tool environment (see the module docs) in place of the app's own.
pub fn apply(command: &mut Command, search: &[PathBuf]) {
    command
        .env_clear()
        .envs(tool_env_from(std::env::vars_os(), search));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn keeps_what_toolchains_need_and_drops_secrets_and_code_loaders() {
        let ambient = pairs(&[
            ("HOME", "/Users/me"),
            ("LC_ALL", "de_DE.UTF-8"),
            ("CARGO_HOME", "/Users/me/.cargo"),
            ("VIRTUAL_ENV", "/p/.venv"),
            ("OPENROUTER_API_KEY", "sk-secret"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("NODE_OPTIONS", "--require /tmp/evil.js"),
            ("PATH", "/somewhere/else"),
        ]);
        let search = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
        ];
        let env = tool_env_from(ambient, &search);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert_eq!(
            keys,
            ["HOME", "LC_ALL", "CARGO_HOME", "VIRTUAL_ENV", "PATH"]
        );
        assert_eq!(
            env.last().unwrap().1,
            OsString::from("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        );
    }
}
