//! Where a plugin comes from (plugins.md Q1): the newest release of a GitHub repository (`owner/repo`), a direct
//! address of a release's `plugin.json` with its `plugin.wasm` beside it, or a folder on this Mac while a plugin is
//! being written. Only HTTPS, every redirect HTTPS too, every download within a size.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::manifest::{MANIFEST, WASM};
use crate::{Error, Result};

/// The most a manifest and a component may weigh when fetched.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The most a component may weigh: it is translated into machine code before the user is asked, which takes time
/// and memory with its size. A plugin with QuickJS inside weighs 1.4 MiB.
pub(crate) const MAX_WASM_BYTES: u64 = 16 * 1024 * 1024;
/// GitHub's answer about a release, which lists every asset.
const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 5;
/// No name or address of a person rides along: the same words for everyone.
const USER_AGENT: &str = concat!("axiomata-plugin/", env!("CARGO_PKG_VERSION"));
/// The GitHub account whose plugins the apps show as "Von Axiomata" (plugins.md Q4).
pub const OURS: &str = "axiomata-labs";

/// Where a plugin comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Source {
    /// The newest release of `owner/repo` on GitHub.
    GitHub { owner: String, repo: String },
    /// A release's `plugin.json` at an HTTPS address; `plugin.wasm` beside it.
    Address { url: String },
    /// A folder holding `plugin.json` and `plugin.wasm`, while a plugin is being written.
    Folder { path: PathBuf },
}

impl Source {
    /// Reads what the user typed: `owner/repo`, `https://github.com/owner/repo`, an HTTPS address of a
    /// `plugin.json`, or a folder (absolute, under `~`, or starting with `./` or `../`).
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("https://github.com/") {
            let mut parts = rest.trim_end_matches('/').split('/');
            if let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) {
                return Self::github(owner, repo.trim_end_matches(".git"));
            }
        }
        if text.starts_with("https://") {
            let url = reqwest::Url::parse(text)
                .map_err(|err| Error::Source(format!("not an address: {err}")))?;
            if !url.path().ends_with(&format!("/{MANIFEST}"))
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(Error::Source(format!(
                    "an address must lead to a release's {MANIFEST}, and only that"
                )));
            }
            return Ok(Self::Address {
                url: url.to_string(),
            });
        }
        if text.starts_with("http://") {
            return Err(Error::Source("only HTTPS addresses".into()));
        }
        if text.starts_with('/')
            || text.starts_with("~/")
            || text.starts_with("./")
            || text.starts_with("../")
        {
            let written = match text.strip_prefix("~/") {
                Some(rest) => home()?.join(rest),
                None => PathBuf::from(text),
            };
            // One folder is one source however it was typed: `~/x`, `./x` and its full path alike.
            let path = std::fs::canonicalize(&written)
                .or_else(|_| std::path::absolute(&written))
                .map_err(|err| Error::Source(format!("{text}: {err}")))?;
            return Ok(Self::Folder { path });
        }
        let mut parts = text.split('/');
        if let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) {
            return Self::github(owner, repo.trim_end_matches(".git"));
        }
        Err(Error::Source(format!(
            "{text:?} is no owner/repo, HTTPS address or folder"
        )))
    }

    fn github(owner: &str, repo: &str) -> Result<Self> {
        let fits = |part: &str| {
            (1..=100).contains(&part.len())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && !part.starts_with('.')
        };
        if !fits(owner) || !fits(repo) {
            return Err(Error::Source(format!(
                "{owner}/{repo} is not a GitHub repository's name"
            )));
        }
        Ok(Self::GitHub {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }

    /// Who publishes from here, as the app shows it: the GitHub account, the address's host, or "local".
    pub fn publisher(&self) -> String {
        match self {
            Self::GitHub { owner, .. } => owner.clone(),
            // The host and the first part of the path: on a shared host (github.com, a pages site) the path, not
            // the host, says whose it is.
            Self::Address { url } => reqwest::Url::parse(url)
                .ok()
                .map(|url| {
                    let first = url
                        .path_segments()
                        .and_then(|mut parts| parts.next())
                        .unwrap_or_default();
                    let host = url.host_str().unwrap_or_default();
                    if first.is_empty() || first == MANIFEST {
                        host.to_owned()
                    } else {
                        format!("{host}/{first}")
                    }
                })
                .unwrap_or_default(),
            Self::Folder { .. } => "local".to_owned(),
        }
    }

    /// Whether it is one of ours: only a GitHub repository of [`OURS`] — never what a manifest claims.
    pub fn is_ours(&self) -> bool {
        matches!(self, Self::GitHub { owner, .. } if owner.eq_ignore_ascii_case(OURS))
    }

    /// Where the user can look at it.
    pub fn shown(&self) -> String {
        match self {
            Self::GitHub { owner, repo } => format!("github.com/{owner}/{repo}"),
            Self::Address { url } => url.clone(),
            Self::Folder { path } => path.display().to_string(),
        }
    }

    /// Fetches the manifest's and the component's bytes, and the release's tag if there is one.
    pub(crate) fn fetch(&self) -> Result<Fetched> {
        match self {
            Self::Folder { path } => Ok(Fetched {
                manifest: read_file(&path.join(MANIFEST), MAX_MANIFEST_BYTES)?,
                wasm: read_file(&path.join(WASM), MAX_WASM_BYTES)?,
            }),
            Self::Address { url } => {
                let client = client()?;
                let wasm_url = format!("{}{WASM}", url.trim_end_matches(MANIFEST));
                Ok(Fetched {
                    manifest: download(&client, url, MAX_MANIFEST_BYTES)?,
                    wasm: download(&client, &wasm_url, MAX_WASM_BYTES)?,
                })
            }
            Self::GitHub { owner, repo } => {
                let client = client()?;
                let api = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
                let release: Release =
                    serde_json::from_slice(&download(&client, &api, MAX_RELEASE_BYTES)?)
                        .map_err(|err| fetch_failed(&api, &err.to_string()))?;
                // Only assets of this repository's releases on github.com: a repository that moved elsewhere, or an
                // answer that points away, is no release of what the user typed.
                let own =
                    format!("https://github.com/{owner}/{repo}/releases/download/").to_lowercase();
                let asset = |name: &str| {
                    release
                        .assets
                        .iter()
                        .find(|asset| asset.name == name)
                        .map(|asset| &asset.browser_download_url)
                        .filter(|url| url.to_lowercase().starts_with(&own))
                };
                let (Some(manifest), Some(wasm)) = (asset(MANIFEST), asset(WASM)) else {
                    return Err(Error::Source(format!(
                        "the newest release of {owner}/{repo} ({:?}) has no {MANIFEST} and {WASM} of its own",
                        release.tag_name.chars().take(64).collect::<String>()
                    )));
                };
                Ok(Fetched {
                    manifest: download(&client, manifest, MAX_MANIFEST_BYTES)?,
                    wasm: download(&client, wasm, MAX_WASM_BYTES)?,
                })
            }
        }
    }
}

/// What a source gave.
pub(crate) struct Fetched {
    pub manifest: Vec<u8>,
    pub wasm: Vec<u8>,
}

/// The part of GitHub's answer about a release that is read.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn client() -> Result<reqwest::blocking::Client> {
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if attempt.url().scheme() != "https" {
            attempt.error("a redirect away from HTTPS")
        } else {
            attempt.follow()
        }
    });
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(TIMEOUT)
        .redirect(redirects)
        .https_only(true)
        .build()
        .map_err(|err| Error::Source(format!("no HTTPS client: {err}")))
}

fn download(client: &reqwest::blocking::Client, url: &str, most: u64) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| fetch_failed(url, &err.to_string()))?;
    if response
        .content_length()
        .is_some_and(|length| length > most)
    {
        return Err(fetch_failed(url, &format!("larger than {most} bytes")));
    }
    let mut bytes = Vec::new();
    response
        .take(most + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| fetch_failed(url, &err.to_string()))?;
    if bytes.len() as u64 > most {
        return Err(fetch_failed(url, &format!("larger than {most} bytes")));
    }
    Ok(bytes)
}

fn read_file(path: &Path, most: u64) -> Result<Vec<u8>> {
    let failed = |why: String| Error::Fetch {
        what: path.display().to_string(),
        why,
    };
    let length = std::fs::metadata(path)
        .map_err(|err| failed(err.to_string()))?
        .len();
    if length > most {
        return Err(failed(format!("larger than {most} bytes")));
    }
    std::fs::read(path).map_err(|err| failed(err.to_string()))
}

fn fetch_failed(what: &str, why: &str) -> Error {
    Error::Fetch {
        what: what.to_owned(),
        why: why.to_owned(),
    }
}

fn home() -> Result<PathBuf> {
    std::env::home_dir().ok_or_else(|| Error::Source("no home folder".into()))
}
