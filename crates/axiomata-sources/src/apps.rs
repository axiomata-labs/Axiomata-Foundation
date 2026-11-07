//! The apps on the Mac and web addresses (§8k Q7/Q18): which apps there are — from
//! `/Applications`, `/System/Applications` and `~/Applications` — open one by its bundle identifier, never by a path
//! a caller names; open an `https` address in the browser; quit gently what was opened. A fixed list of apps is never
//! opened: shells and terminals, script and automation tools, System Settings, the keychain and passwords, and tools
//! that change the disk or reach another Mac.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Apps never opened, by bundle identifier: what runs commands, changes the system or shows secrets.
pub const BLOCKED: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "io.alacritty",
    "org.alacritty",
    "com.github.wez.wezterm",
    "co.zeit.hyper",
    "com.apple.ScriptEditor2",
    "com.apple.Automator",
    "com.apple.systempreferences",
    "com.apple.SystemPreferences",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.apple.Console",
    "com.apple.shortcuts",
    "com.apple.ActivityMonitor",
    "com.apple.DiskUtility",
    "com.apple.MigrateAssistant",
    "com.apple.ScreenSharing",
    "com.apple.RemoteDesktop",
    "com.latenightsw.ScriptDebugger8",
    "com.latenightsw.ScriptDebugger7",
    "org.tabby",
    "com.termius-dmg.mac",
    "com.raphaelamorim.rio",
    "org.hammerspoon.Hammerspoon",
    "com.stairways.keyboardmaestro.engine",
    "com.stairways.keyboardmaestro.editor",
];
/// The longest web address opened.
const MAX_URL_CHARS: usize = 2000;

/// An app on the Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// Its name as the Finder shows it ("Safari").
    pub name: String,
    pub bundle: String,
    pub path: PathBuf,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppError {
    /// It is one of [`BLOCKED`] or of the owner's own list.
    Blocked,
    /// No such app, or an address that is no plain `https` one.
    Invalid(&'static str),
    Failed(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Blocked => write!(f, "that app is never opened"),
            AppError::Invalid(why) => write!(f, "{why}"),
            AppError::Failed(why) => write!(f, "it could not be opened: {why}"),
        }
    }
}

impl std::error::Error for AppError {}

/// The folders apps are found in.
fn folders() -> Vec<PathBuf> {
    let mut folders = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = std::env::home_dir() {
        folders.push(home.join("Applications"));
    }
    folders
}

/// The apps there are, by name; one of a name only once (the first folder wins).
pub fn installed() -> Vec<App> {
    let mut apps: Vec<App> = Vec::new();
    for folder in folders() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ending| ending != "app") {
                continue;
            }
            let Some(name) = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
            else {
                continue;
            };
            if name
                .chars()
                .any(|c| c.is_control() || crate::calendar::invisible(c))
                || apps.iter().any(|app| app.name.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            if let Some(bundle) = imp::bundle(&path) {
                apps.push(App { name, bundle, path });
            }
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// Whether `bundle` may not be opened: one of [`BLOCKED`], or of the owner's `blocked`.
pub fn is_blocked(bundle: &str, blocked: &[String]) -> bool {
    BLOCKED
        .iter()
        .any(|known| known.eq_ignore_ascii_case(bundle))
        || blocked
            .iter()
            .any(|known| known.trim().eq_ignore_ascii_case(bundle))
}

/// Whether an app of `bundle` runs.
pub fn running(bundle: &str) -> bool {
    imp::running(bundle)
}

/// Opens `app` — the very bundle [`installed`] found, not whatever else claims its identifier — unless blocked.
///
/// # Errors
///
/// [`AppError::Blocked`], [`AppError::Invalid`] for an app no longer where it was found, [`AppError::Failed`].
pub fn open(app: &App, blocked: &[String]) -> Result<(), AppError> {
    if is_blocked(&app.bundle, blocked) {
        return Err(AppError::Blocked);
    }
    // Still in one of the apps' folders, and still that app.
    let here = std::fs::canonicalize(&app.path).map_err(|_| AppError::Invalid("no such app"))?;
    let in_folder = folders()
        .iter()
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .any(|folder| here.parent() == Some(folder.as_path()));
    if !in_folder || imp::bundle(&here).as_deref() != Some(app.bundle.as_str()) {
        return Err(AppError::Invalid("no such app"));
    }
    // The path as one argument, never through a shell.
    run(Command::new("/usr/bin/open").arg("-a").arg(&here))
}

/// Quits the app of `bundle` gently — as ⌘Q does: it may ask to save. Whether it was asked to.
pub fn quit(bundle: &str) -> bool {
    imp::quit(bundle)
}

/// The host of `url` when it is a plain `https` address of a named host on the internet: no user part, no address of
/// this Mac or the home network (`localhost`, `*.local`, an IP literal, a name without a dot), no host in another
/// script than plain letters — what a caller says aloud before it is opened.
///
/// # Errors
///
/// [`AppError::Invalid`] for anything else.
pub fn https_host(url: &str) -> Result<String, AppError> {
    let url = url.trim();
    let host = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or_default()
        .to_lowercase();
    let name = host.split(':').next().unwrap_or_default().to_owned();
    let ip = name.chars().all(|c| c.is_ascii_digit() || c == '.') || host.starts_with('[');
    if url.chars().count() > MAX_URL_CHARS
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || crate::calendar::invisible(c))
        || name.is_empty()
        || host.contains('@')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        || ip
        || name == "localhost"
        || name.ends_with(".local")
        || name.ends_with(".localhost")
        || name.starts_with("xn--")
        || name.contains(".xn--")
        || !name.contains('.')
    {
        return Err(AppError::Invalid("only a plain https address"));
    }
    Ok(name)
}

/// Opens `url` in the browser: a plain `https` address only (see [`https_host`]).
///
/// # Errors
///
/// [`AppError::Invalid`] for anything else, [`AppError::Failed`].
pub fn open_url(url: &str) -> Result<(), AppError> {
    https_host(url)?;
    run(Command::new("/usr/bin/open").arg("--").arg(url.trim()))
}

fn run(command: &mut Command) -> Result<(), AppError> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| AppError::Failed(err.to_string()))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| AppError::Failed("macOS refused".into()))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::{NSBundle, NSString};

    pub(super) fn bundle(path: &Path) -> Option<String> {
        let path = NSString::from_str(path.to_str()?);
        let bundle = NSBundle::bundleWithPath(&path)?;
        bundle.bundleIdentifier().map(|id| id.to_string())
    }

    pub(super) fn running(bundle: &str) -> bool {
        let found = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle),
        );
        found.iter().any(|app| !app.isTerminated())
    }

    pub(super) fn quit(bundle: &str) -> bool {
        let found = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle),
        );
        let mut asked = false;
        for app in found.iter().filter(|app| !app.isTerminated()) {
            asked |= app.terminate();
        }
        asked
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::Path;

    pub(super) fn bundle(_path: &Path) -> Option<String> {
        None
    }
    pub(super) fn running(_bundle: &str) -> bool {
        false
    }
    pub(super) fn quit(_bundle: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_and_secrets_stay_closed_and_only_plain_https_opens() {
        assert!(is_blocked("com.apple.Terminal", &[]));
        assert!(is_blocked("COM.APPLE.TERMINAL", &[]));
        assert!(is_blocked("com.example.x", &["com.example.X".into()]));
        assert!(!is_blocked("com.apple.Safari", &[]));
        let app = |bundle: &str, path: &str| App {
            name: "x".into(),
            bundle: bundle.into(),
            path: PathBuf::from(path),
        };
        let terminal = app(
            "com.apple.Terminal",
            "/System/Applications/Utilities/Terminal.app",
        );
        assert_eq!(open(&terminal, &[]), Err(AppError::Blocked));
        assert_eq!(
            open(&app("com.example.x", "/tmp/x.app"), &[]),
            Err(AppError::Invalid("no such app"))
        );
        assert_eq!(
            https_host("https://www.Example.com/a?b=c"),
            Ok("www.example.com".into())
        );
        for bad in [
            "http://example.com",
            "file:///etc/passwd",
            "https://",
            "https://user@example.com",
            "https://exa mple.com",
            "javascript:alert(1)",
            "https://example.com/\u{202e}x",
            "https://192.168.1.1/admin",
            "https://localhost:8443/",
            "https://drucker.local/",
            "https://[::1]/",
            "https://xn--80ak6aa92e.com/",
            "https://intranet/",
            "HTTPS://example.com",
        ] {
            assert_eq!(
                open_url(bad),
                Err(AppError::Invalid("only a plain https address")),
                "{bad}"
            );
        }
    }

    #[test]
    #[ignore = "reads this Mac's apps; run by hand"]
    fn on_this_mac() {
        let started = std::time::Instant::now();
        let apps = installed();
        eprintln!("{} apps in {:?}", apps.len(), started.elapsed());
        assert!(
            apps.iter()
                .any(|app| app.bundle == "com.apple.Safari" || app.name == "Safari")
        );
    }
}
