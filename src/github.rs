//! Sync with a private GitHub repository using a shared team key.
//!
//! The app works against a local folder as usual (a mirror that the app owns).
//! This module keeps the mirror and the repository equal: new files in the
//! repository are downloaded, new local files are uploaded. Since every event
//! is its own file that never changes, the content can never conflict.
//!
//! Everything coming from the repository is untrusted input: only paths of the
//! form `ops/YYYY-MM/<valid file name>` are accepted, and the size is limited.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::blobs;
use crate::i18n::t;
use crate::ops::{self, FORMAT_FILE, FORMAT_VERSION, MAX_FILE_BYTES, OPS_DIR};

const API_BASE: &str = "https://api.github.com";
/// How often we ask GitHub whether there is anything new.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);
const UPLOAD_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubConfig {
    /// `owner/repo`
    pub repo: String,
    pub token: String,
    pub branch: String,
}

/// The team key is `owner/repo:token` (a leading `simq:` is allowed).
pub fn parse_team_key(key: &str) -> Result<(String, String), String> {
    let bad = || {
        t(
            "Teamnyckeln ska se ut som ägare/repo:token.",
            "The team key should look like owner/repo:token.",
        )
        .to_string()
    };
    let key = key.trim();
    let key = key.strip_prefix("simq:").unwrap_or(key);
    let (repo, token) = key.split_once(':').ok_or_else(bad)?;
    let (repo, token) = (repo.trim(), token.trim());
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let (owner, name) = repo.split_once('/').ok_or_else(bad)?;
    if !name_ok(owner) || !name_ok(name) {
        return Err(bad());
    }
    if token.len() < 10 || token.len() > 400 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad());
    }
    Ok((repo.to_string(), token.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// No contact (no network, timeout).
    Offline(String),
    /// The key is not accepted or lacks permissions.
    Denied,
    /// The repository does not exist, or the key cannot reach it.
    NotFound,
    /// Someone else wrote at the same time. Try again.
    Conflict,
    /// The file already exists.
    Exists,
    /// An uploaded file is not in the store (yet).
    FileMissing,
    Other(String),
}

impl RemoteError {
    pub fn text(&self) -> String {
        match self {
            RemoteError::Offline(e) => format!(
                "{}: {e}",
                t("Ingen kontakt med GitHub", "No contact with GitHub")
            ),
            RemoteError::Denied => t(
                "GitHub nekade åtkomst. Kontrollera att teamnyckeln är rätt och inte har gått ut.",
                "GitHub denied access. Check that the team key is correct and has not expired.",
            )
            .to_string(),
            RemoteError::NotFound => t(
                "Repot hittas inte, eller så har teamnyckeln inte åtkomst till det.",
                "The repository cannot be found, or the team key does not have access to it.",
            )
            .to_string(),
            RemoteError::Conflict => t(
                "GitHub var upptaget. Försöker igen.",
                "GitHub was busy. Trying again.",
            )
            .to_string(),
            RemoteError::FileMissing => t(
                "Filen finns inte uppladdad än. Den som lade upp den kanske fortfarande laddar upp. Försök igen om en stund.",
                "The file has not been uploaded yet. Whoever added it may still be uploading. Try again in a moment.",
            )
            .to_string(),
            RemoteError::Exists => t("Filen finns redan.", "The file already exists.").to_string(),
            RemoteError::Other(e) => format!("{}: {e}", t("Fel från GitHub", "GitHub error")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Nothing has changed since the previous listing.
    Unchanged,
    Files {
        etag: Option<String>,
        files: Vec<RemoteFile>,
    },
}

/// What is needed from a remote store. GitHub in production, a fake store in tests.
pub trait RemoteStore: Send {
    fn list(&mut self, etag: Option<&str>) -> Result<Listing, RemoteError>;
    fn download(&mut self, path: &str) -> Result<Vec<u8>, RemoteError>;
    /// Creates a NEW file. `Exists` if it is already there.
    fn upload(&mut self, path: &str, bytes: &[u8]) -> Result<(), RemoteError>;
    /// Uploads a user file under its SHA-256. Does nothing if it is already there.
    fn upload_file(&mut self, sha256: &str, path: &Path) -> Result<(), RemoteError>;
    /// Downloads a user file to `dest`, accepting at most `size` bytes.
    fn download_file(
        &mut self,
        sha256: &str,
        size: u64,
        dest: &Path,
        progress: &AtomicU64,
    ) -> Result<(), RemoteError>;
}

// ---------------------------------------------------------------------------
// The GitHub REST API
// ---------------------------------------------------------------------------

pub struct GithubRemote {
    agent: ureq::Agent,
    /// For file transfers, which may take a long time.
    transfer: ureq::Agent,
    base: String,
    repo: String,
    token: String,
    branch: String,
}

#[derive(Deserialize)]
struct RepoInfo {
    default_branch: String,
    #[serde(default)]
    permissions: Option<RepoPermissions>,
}

#[derive(Deserialize)]
struct RepoPermissions {
    #[serde(default)]
    push: bool,
}

#[derive(Deserialize)]
struct TreeResponse {
    #[serde(default)]
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    sha: String,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(25))
        .build()
}

fn classify(error: ureq::Error) -> RemoteError {
    match error {
        ureq::Error::Status(code, response) => {
            let limited = response.header("x-ratelimit-remaining") == Some("0");
            match code {
                429 => RemoteError::Other(
                    t("för många anrop just nu", "too many requests right now").to_string(),
                ),
                403 if limited => RemoteError::Other(
                    t("för många anrop just nu", "too many requests right now").to_string(),
                ),
                401 | 403 => RemoteError::Denied,
                404 => RemoteError::NotFound,
                409 => RemoteError::Conflict,
                422 => RemoteError::Exists,
                _ => RemoteError::Other(format!("HTTP {code}")),
            }
        }
        ureq::Error::Transport(transport) => RemoteError::Offline(transport.to_string()),
    }
}

/// Percent-encodes everything except safe characters (branch names may contain e.g. `/`).
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Only paths the app creates itself may be sent to the API.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("..")
        && !path.starts_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

impl GithubRemote {
    pub fn new(config: &GithubConfig) -> Self {
        Self::with_base(API_BASE, config)
    }

    pub fn with_base(base: &str, config: &GithubConfig) -> Self {
        GithubRemote {
            agent: agent(),
            transfer: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(8))
                .timeout_read(Duration::from_secs(120))
                .timeout_write(Duration::from_secs(120))
                .build(),
            base: base.trim_end_matches('/').to_string(),
            repo: config.repo.clone(),
            token: config.token.clone(),
            branch: config.branch.clone(),
        }
    }

    /// Checks that the key can reach the repository and may write. Returns the
    /// repository's default branch.
    pub fn check(repo: &str, token: &str) -> Result<String, RemoteError> {
        Self::check_at(API_BASE, repo, token)
    }

    pub fn check_at(base: &str, repo: &str, token: &str) -> Result<String, RemoteError> {
        let probe = Self::with_base(
            base,
            &GithubConfig {
                repo: repo.to_string(),
                token: token.to_string(),
                branch: String::new(),
            },
        );
        let url = format!("{}/repos/{}", probe.base, probe.repo);
        let response = probe.request("GET", &url).call().map_err(classify)?;
        let body = response
            .into_string()
            .map_err(|e| RemoteError::Offline(e.to_string()))?;
        let info: RepoInfo =
            serde_json::from_str(&body).map_err(|e| RemoteError::Other(e.to_string()))?;
        if matches!(info.permissions, Some(RepoPermissions { push: false })) {
            return Err(RemoteError::Other(
                t(
                    "teamnyckeln får bara läsa, inte skriva",
                    "the team key can only read, not write",
                )
                .to_string(),
            ));
        }
        Ok(info.default_branch)
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        self.headers(self.agent.request(method, url))
    }

    fn headers(&self, request: ureq::Request) -> ureq::Request {
        request
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .set("User-Agent", "simq")
    }
}

#[derive(Deserialize)]
struct ReleaseInfo {
    id: u64,
    upload_url: String,
}

#[derive(Deserialize)]
struct AssetInfo {
    id: u64,
    name: String,
    url: String,
    size: u64,
    #[serde(default)]
    state: String,
}

/// User files are stored as release assets, spread over up to 256 releases
/// (one per first byte of the SHA-256) to stay well below GitHub's limit on
/// the number of files per release.
fn file_bucket(sha256: &str) -> String {
    format!("simq-files-{}", &sha256[..2])
}

fn parse_json<T: serde::de::DeserializeOwned>(response: ureq::Response) -> Result<T, RemoteError> {
    let body = response
        .into_string()
        .map_err(|e| RemoteError::Offline(e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| RemoteError::Other(e.to_string()))
}

impl GithubRemote {
    /// Finds the release that holds files for `tag`, creating it if asked to.
    fn file_release(&self, tag: &str, create: bool) -> Result<Option<ReleaseInfo>, RemoteError> {
        let by_tag = format!("{}/repos/{}/releases/tags/{tag}", self.base, self.repo);
        match self.request("GET", &by_tag).call() {
            Ok(response) => return parse_json(response).map(Some),
            Err(ureq::Error::Status(404, _)) if create => {}
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(e) => return Err(classify(e)),
        }
        let body = serde_json::json!({
            "tag_name": tag,
            "target_commitish": self.branch,
            "name": format!("SimQ files {}", tag.rsplit('-').next().unwrap_or_default()),
            "body": "Files uploaded through SimQ. Managed by the app, do not edit by hand.",
            "prerelease": true,
        });
        let url = format!("{}/repos/{}/releases", self.base, self.repo);
        match self
            .request("POST", &url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
        {
            Ok(response) => parse_json(response).map(Some),
            // Someone else created it at the same moment.
            Err(ureq::Error::Status(422, _)) => self
                .request("GET", &by_tag)
                .call()
                .map_err(classify)
                .and_then(parse_json)
                .map(Some),
            Err(e) => Err(classify(e)),
        }
    }

    fn find_asset(&self, release_id: u64, name: &str) -> Result<Option<AssetInfo>, RemoteError> {
        for page in 1..=20 {
            let url = format!(
                "{}/repos/{}/releases/{release_id}/assets?per_page=100&page={page}",
                self.base, self.repo
            );
            let assets: Vec<AssetInfo> =
                parse_json(self.request("GET", &url).call().map_err(classify)?)?;
            if assets.is_empty() {
                break;
            }
            if let Some(asset) = assets.into_iter().find(|asset| asset.name == name) {
                return Ok(Some(asset));
            }
        }
        Ok(None)
    }
}

impl RemoteStore for GithubRemote {
    fn upload_file(&mut self, sha256: &str, path: &Path) -> Result<(), RemoteError> {
        if !blobs::valid_sha256(sha256) {
            return Err(RemoteError::Other(
                t("ogiltig sökväg", "invalid path").to_string(),
            ));
        }
        let size = std::fs::metadata(path)
            .map_err(|e| RemoteError::Other(e.to_string()))?
            .len();
        let release = self
            .file_release(&file_bucket(sha256), true)?
            .ok_or(RemoteError::NotFound)?;
        let complete = |asset: &AssetInfo| asset.state == "uploaded" && asset.size == size;
        if let Some(asset) = self.find_asset(release.id, sha256)? {
            if complete(&asset) {
                return Ok(());
            }
            // A previous upload was interrupted: remove the remains and start over.
            let url = format!(
                "{}/repos/{}/releases/assets/{}",
                self.base, self.repo, asset.id
            );
            self.request("DELETE", &url).call().map_err(classify)?;
        }
        let upload = release.upload_url.split('{').next().unwrap_or_default();
        let url = format!("{upload}?name={sha256}");
        let file = std::fs::File::open(path).map_err(|e| RemoteError::Other(e.to_string()))?;
        let sent = self
            .headers(self.transfer.request("POST", &url))
            .set("Content-Type", "application/octet-stream")
            .set("Content-Length", &size.to_string())
            .send(file);
        match sent.map_err(classify) {
            Ok(_) => Ok(()),
            // Someone else uploaded the same file at the same moment.
            Err(RemoteError::Exists) => match self.find_asset(release.id, sha256)? {
                Some(asset) if complete(&asset) => Ok(()),
                _ => Err(RemoteError::Conflict),
            },
            Err(e) => Err(e),
        }
    }

    fn download_file(
        &mut self,
        sha256: &str,
        size: u64,
        dest: &Path,
        progress: &AtomicU64,
    ) -> Result<(), RemoteError> {
        if !blobs::valid_sha256(sha256) {
            return Err(RemoteError::Other(
                t("ogiltig sökväg", "invalid path").to_string(),
            ));
        }
        let Some(release) = self.file_release(&file_bucket(sha256), false)? else {
            return Err(RemoteError::FileMissing);
        };
        let Some(asset) = self
            .find_asset(release.id, sha256)?
            .filter(|asset| asset.state == "uploaded")
        else {
            return Err(RemoteError::FileMissing);
        };
        let response = self
            .headers(self.transfer.get(&asset.url))
            .set("Accept", "application/octet-stream")
            .call()
            .map_err(classify)?;
        let save = |e: std::io::Error| RemoteError::Other(e.to_string());
        let mut reader = response.into_reader().take(size + 1);
        let mut file = std::fs::File::create(dest).map_err(save)?;
        let mut buffer = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|e| RemoteError::Offline(e.to_string()))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > size {
                return Err(RemoteError::Other(
                    t(
                        "filen är större än väntat",
                        "the file is larger than expected",
                    )
                    .to_string(),
                ));
            }
            std::io::Write::write_all(&mut file, &buffer[..n]).map_err(save)?;
            progress.store(total, Ordering::Relaxed);
        }
        file.sync_all().map_err(save)
    }

    fn list(&mut self, etag: Option<&str>) -> Result<Listing, RemoteError> {
        let url = format!(
            "{}/repos/{}/git/trees/{}?recursive=1",
            self.base,
            self.repo,
            encode(&self.branch)
        );
        let mut request = self.request("GET", &url);
        if let Some(etag) = etag {
            request = request.set("If-None-Match", etag);
        }
        let response = match request.call() {
            Ok(response) => response,
            // A completely empty repository has no tree yet.
            Err(ureq::Error::Status(409, _)) => {
                return Ok(Listing::Files {
                    etag: None,
                    files: Vec::new(),
                })
            }
            Err(e) => return Err(classify(e)),
        };
        if response.status() == 304 {
            return Ok(Listing::Unchanged);
        }
        let etag = response.header("ETag").map(str::to_string);
        let body = response
            .into_string()
            .map_err(|e| RemoteError::Offline(e.to_string()))?;
        let tree: TreeResponse =
            serde_json::from_str(&body).map_err(|e| RemoteError::Other(e.to_string()))?;
        if tree.truncated {
            return Err(RemoteError::Other(
                t(
                    "repot innehåller för många filer",
                    "the repository contains too many files",
                )
                .to_string(),
            ));
        }
        let files = tree
            .tree
            .into_iter()
            .filter(|entry| entry.kind == "blob")
            .map(|entry| RemoteFile {
                path: entry.path,
                size: entry.size.unwrap_or(0),
                sha: entry.sha,
            })
            .collect();
        Ok(Listing::Files { etag, files })
    }

    fn download(&mut self, path: &str) -> Result<Vec<u8>, RemoteError> {
        if !safe_path(path) {
            return Err(RemoteError::Other(
                t("ogiltig sökväg", "invalid path").to_string(),
            ));
        }
        let url = format!(
            "{}/repos/{}/contents/{}?ref={}",
            self.base,
            self.repo,
            path,
            encode(&self.branch)
        );
        let response = self
            .request("GET", &url)
            .set("Accept", "application/vnd.github.raw+json")
            .call()
            .map_err(classify)?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| RemoteError::Offline(e.to_string()))?;
        Ok(bytes)
    }

    fn upload(&mut self, path: &str, bytes: &[u8]) -> Result<(), RemoteError> {
        if !safe_path(path) {
            return Err(RemoteError::Other(
                t("ogiltig sökväg", "invalid path").to_string(),
            ));
        }
        let url = format!("{}/repos/{}/contents/{}", self.base, self.repo, path);
        let name = path.rsplit('/').next().unwrap_or(path);
        let body = serde_json::json!({
            "message": format!("simq: {name}"),
            "content": base64_encode(bytes),
            "branch": self.branch,
        });
        self.request("PUT", &url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map(|_| ())
            .map_err(classify)
    }
}

// ---------------------------------------------------------------------------
// Mirroring
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct MirrorState {
    etag: Option<String>,
    /// Paths we know exist in the repository.
    known: HashSet<String>,
    /// True once we have listed the repository at least once (only then do we know what is missing there).
    listed: bool,
    format_sha: Option<String>,
    /// The repository requires a newer app: then we upload nothing.
    remote_newer: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MirrorReport {
    pub downloaded: usize,
    pub uploaded: usize,
    /// Local events that are not yet in the repository.
    pub pending: usize,
    pub files_uploaded: usize,
    /// Local user files that are not yet uploaded.
    pub pending_files: usize,
    pub error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct FormatProbe {
    format_version: u32,
}

/// `ops/YYYY-MM/<valid file name>` → (month folder, file name). Anything else gives `None`.
fn op_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("ops/")?;
    let (dir, name) = rest.split_once('/')?;
    let bytes = dir.as_bytes();
    let dir_ok = bytes.len() == 7
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || b.is_ascii_digit());
    if dir_ok && ops::parse_file_name(name).is_some() {
        Some((dir, name))
    } else {
        None
    }
}

fn read_limited(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

fn upload_with_retry(
    remote: &mut dyn RemoteStore,
    path: &str,
    bytes: &[u8],
) -> Result<(), RemoteError> {
    let mut last = RemoteError::Conflict;
    for _ in 0..UPLOAD_ATTEMPTS {
        match remote.upload(path, bytes) {
            Ok(()) | Err(RemoteError::Exists) => return Ok(()),
            Err(RemoteError::Conflict) => last = RemoteError::Conflict,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// One round: fetch what is new in the repository, send what is new locally.
pub fn mirror_once(
    remote: &mut dyn RemoteStore,
    folder: &Path,
    state: &mut MirrorState,
) -> MirrorReport {
    let mut report = MirrorReport::default();
    let ops_dir = folder.join(OPS_DIR);
    let _ = std::fs::create_dir_all(&ops_dir);

    match remote.list(state.etag.as_deref()) {
        Ok(Listing::Unchanged) => {}
        Ok(Listing::Files { etag, files }) => {
            state.known = files.iter().map(|f| f.path.clone()).collect();
            state.listed = true;
            let mut complete = true;
            for file in &files {
                if file.path == FORMAT_FILE {
                    if state.format_sha.as_deref() != Some(file.sha.as_str()) {
                        match remote.download(FORMAT_FILE) {
                            Ok(bytes) => {
                                state.format_sha = Some(file.sha.clone());
                                let version = serde_json::from_slice::<FormatProbe>(&bytes)
                                    .map(|probe| probe.format_version)
                                    .unwrap_or(0);
                                state.remote_newer = version > FORMAT_VERSION;
                                if state.remote_newer {
                                    // The mirror is the app's own: let it show that the repository
                                    // requires a newer app, so that nothing is written.
                                    let _ = std::fs::write(folder.join(FORMAT_FILE), &bytes);
                                }
                            }
                            Err(e) => {
                                report.error = Some(e);
                                complete = false;
                                break;
                            }
                        }
                    }
                    continue;
                }
                let Some((dir, name)) = op_path(&file.path) else {
                    continue;
                };
                if file.size > MAX_FILE_BYTES {
                    continue;
                }
                let target_dir = ops_dir.join(dir);
                let target = target_dir.join(name);
                if target.exists() {
                    continue;
                }
                match remote.download(&file.path) {
                    Ok(bytes) if bytes.len() as u64 <= MAX_FILE_BYTES => {
                        let written = std::fs::create_dir_all(&target_dir)
                            .and_then(|()| ops::write_new_file(&target, &bytes));
                        if written.is_ok() {
                            report.downloaded += 1;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        report.error = Some(e);
                        complete = false;
                        break;
                    }
                }
            }
            // If we were interrupted, ask for the whole list again next time.
            state.etag = if complete { etag } else { None };
        }
        Err(e) => report.error = Some(e),
    }

    let local = ops::scan_ops(folder)
        .map(|scan| scan.entries)
        .unwrap_or_default();
    // User files go first, so that an event never points to a file that is not there yet.
    if report.error.is_none() && state.listed && !state.remote_newer {
        for (sha256, path) in blobs::local_files(folder) {
            match remote.upload_file(&sha256, &path) {
                Ok(()) => {
                    // Uploaded: the local copy is no longer needed.
                    let _ = std::fs::remove_file(&path);
                    report.files_uploaded += 1;
                }
                Err(e) => {
                    report.error = Some(e);
                    break;
                }
            }
        }
    }
    report.pending_files = blobs::local_files(folder).len();
    if report.error.is_none() && state.listed && !state.remote_newer {
        if !state.known.contains(FORMAT_FILE) {
            if let Some(bytes) = read_limited(&folder.join(FORMAT_FILE)) {
                match upload_with_retry(remote, FORMAT_FILE, &bytes) {
                    Ok(()) => {
                        state.known.insert(FORMAT_FILE.to_string());
                    }
                    Err(e) => report.error = Some(e),
                }
            }
        }
        for entry in &local {
            if report.error.is_some() {
                break;
            }
            let path = format!("{OPS_DIR}/{}", entry.rel);
            if state.known.contains(&path) {
                continue;
            }
            let Some(bytes) = read_limited(&entry.path) else {
                continue;
            };
            match upload_with_retry(remote, &path, &bytes) {
                Ok(()) => {
                    state.known.insert(path);
                    report.uploaded += 1;
                }
                Err(e) => report.error = Some(e),
            }
        }
    }
    report.pending = local
        .iter()
        .filter(|entry| !state.known.contains(&format!("{OPS_DIR}/{}", entry.rel)))
        .count();
    report
}

// ---------------------------------------------------------------------------
// Background thread
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteState {
    Starting,
    Ok,
    /// No contact: the app works, changes are sent when the network is back.
    Offline,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatus {
    pub repo: String,
    pub state: RemoteState,
    /// Local changes waiting to be sent.
    pub pending: usize,
    /// Uploaded files waiting to be sent.
    pub pending_files: usize,
    pub last_ok_ms: Option<u64>,
}

impl RemoteStatus {
    pub fn starting(repo: &str) -> Self {
        RemoteStatus {
            repo: repo.to_string(),
            state: RemoteState::Starting,
            pending: 0,
            pending_files: 0,
            last_ok_ms: None,
        }
    }
}

/// Handle to the transport thread. The thread stops when the handle is dropped.
pub struct Transport {
    wake: Sender<()>,
    stop: Arc<AtomicBool>,
}

impl Transport {
    /// Run a round now instead of waiting for the next interval.
    pub fn wake(&self) {
        let _ = self.wake.send(());
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
    }
}

/// Starts the thread that mirrors `folder` against `remote`. `report` is called
/// after every round with the status and whether new files were downloaded.
pub fn spawn_transport(
    mut remote: Box<dyn RemoteStore>,
    repo: String,
    folder: PathBuf,
    report: Box<dyn Fn(RemoteStatus, bool) + Send>,
) -> std::io::Result<Transport> {
    let (wake, wake_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("simq-github".to_string())
        .spawn(move || {
            let mut state = MirrorState::default();
            let mut last_ok_ms = None;
            while !stopped.load(Ordering::Relaxed) {
                let round = mirror_once(remote.as_mut(), &folder, &mut state);
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let state_now = match &round.error {
                    None => {
                        last_ok_ms = Some(crate::sync::now_ms());
                        RemoteState::Ok
                    }
                    Some(RemoteError::Offline(_)) => RemoteState::Offline,
                    Some(e) => RemoteState::Error(e.text()),
                };
                report(
                    RemoteStatus {
                        repo: repo.clone(),
                        state: state_now,
                        pending: round.pending,
                        pending_files: round.pending_files,
                        last_ok_ms,
                    },
                    round.downloaded > 0,
                );
                match wake_rx.recv_timeout(POLL_INTERVAL) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                // Several wake-ups in a row only need one round.
                while wake_rx.try_recv().is_ok() {}
            }
        })?;
    Ok(Transport { wake, stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;
    use crate::store::{SeenState, Store};
    use crate::sync::{Client, FolderStatus, Submitted};
    use crate::testutil::{create_action, take_action, TempDir};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use uuid::Uuid;

    // ----- In-memory fake store ---------------------------------------------

    #[derive(Default)]
    struct Repo {
        files: BTreeMap<String, Vec<u8>>,
        version: u64,
        offline: bool,
        conflicts: usize,
        downloads: usize,
        blobs: BTreeMap<String, Vec<u8>>,
    }

    #[derive(Clone, Default)]
    struct FakeRemote(Arc<Mutex<Repo>>);

    impl FakeRemote {
        fn repo(&self) -> std::sync::MutexGuard<'_, Repo> {
            self.0.lock().unwrap()
        }
        fn put(&self, path: &str, bytes: &[u8]) {
            let mut repo = self.repo();
            repo.files.insert(path.to_string(), bytes.to_vec());
            repo.version += 1;
        }
    }

    impl RemoteStore for FakeRemote {
        fn list(&mut self, etag: Option<&str>) -> Result<Listing, RemoteError> {
            let repo = self.repo();
            if repo.offline {
                return Err(RemoteError::Offline("inget nät".to_string()));
            }
            let current = format!("v{}", repo.version);
            if etag == Some(current.as_str()) {
                return Ok(Listing::Unchanged);
            }
            Ok(Listing::Files {
                etag: Some(current),
                files: repo
                    .files
                    .iter()
                    .map(|(path, bytes)| RemoteFile {
                        path: path.clone(),
                        size: bytes.len() as u64,
                        sha: format!("{}-{}", bytes.len(), base64_encode(bytes).len()),
                    })
                    .collect(),
            })
        }

        fn download(&mut self, path: &str) -> Result<Vec<u8>, RemoteError> {
            let mut repo = self.repo();
            if repo.offline {
                return Err(RemoteError::Offline("inget nät".to_string()));
            }
            repo.downloads += 1;
            repo.files.get(path).cloned().ok_or(RemoteError::NotFound)
        }

        fn upload(&mut self, path: &str, bytes: &[u8]) -> Result<(), RemoteError> {
            let mut repo = self.repo();
            if repo.offline {
                return Err(RemoteError::Offline("inget nät".to_string()));
            }
            if repo.conflicts > 0 {
                repo.conflicts -= 1;
                return Err(RemoteError::Conflict);
            }
            if repo.files.contains_key(path) {
                return Err(RemoteError::Exists);
            }
            repo.files.insert(path.to_string(), bytes.to_vec());
            repo.version += 1;
            Ok(())
        }

        fn upload_file(&mut self, sha256: &str, path: &Path) -> Result<(), RemoteError> {
            self.offline()?;
            let bytes = std::fs::read(path).map_err(|e| RemoteError::Other(e.to_string()))?;
            self.repo().blobs.insert(sha256.to_string(), bytes);
            Ok(())
        }

        fn download_file(
            &mut self,
            sha256: &str,
            size: u64,
            dest: &Path,
            progress: &AtomicU64,
        ) -> Result<(), RemoteError> {
            self.offline()?;
            let bytes = self
                .repo()
                .blobs
                .get(sha256)
                .cloned()
                .ok_or(RemoteError::FileMissing)?;
            if bytes.len() as u64 > size {
                return Err(RemoteError::Other("too large".to_string()));
            }
            std::fs::write(dest, &bytes).map_err(|e| RemoteError::Other(e.to_string()))?;
            progress.store(bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        }
    }

    impl FakeRemote {
        fn offline(&self) -> Result<(), RemoteError> {
            if self.repo().offline {
                Err(RemoteError::Offline("inget nät".to_string()))
            } else {
                Ok(())
            }
        }
    }

    /// A team member: client + own mirror folder + own transport to the same repository.
    struct Member {
        dir: TempDir,
        client: Client,
        remote: FakeRemote,
        state: MirrorState,
    }

    impl Member {
        fn new(name: &str, remote: &FakeRemote) -> Member {
            let dir = TempDir::new();
            let client = Client::new(
                Store::open_in_memory().unwrap(),
                Uuid::new_v4(),
                name.to_string(),
                Some(dir.path().to_path_buf()),
            )
            .unwrap();
            let mut member = Member {
                dir,
                client,
                remote: remote.clone(),
                state: MirrorState::default(),
            };
            member.client.sync();
            member
        }

        /// One round against the repository followed by the app reading its folder.
        fn round(&mut self) -> MirrorReport {
            let report = mirror_once(&mut self.remote, self.dir.path(), &mut self.state);
            self.client.sync();
            report
        }

        fn submit(&mut self, id: Option<Uuid>, action: &crate::ops::NewAction) -> Uuid {
            match self.client.submit(id, action) {
                Ok(Submitted::Applied { request_id, .. }) => request_id,
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn two_members_sync_through_the_repo() {
        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        let mut b = Member::new("Bo", &remote);

        let id = a.submit(None, &create_action("Styvhet upright"));
        let report = a.round();
        assert_eq!(
            (report.uploaded, report.pending, report.error),
            (1, 0, None)
        );
        assert!(remote.repo().files.contains_key("format.json"));
        assert_eq!(remote.repo().files.len(), 2);

        let report = b.round();
        assert_eq!((report.downloaded, report.uploaded), (1, 0));
        assert_eq!(b.client.state().requests[&id].title, "Styvhet upright");

        b.submit(Some(id), &take_action("Bo"));
        assert_eq!(b.round().uploaded, 1);
        assert_eq!(a.round().downloaded, 1);
        assert_eq!(a.client.state().requests[&id].status, Status::Running);
        assert_eq!(a.client.state().requests[&id].assignee, "Bo");
        assert_eq!(a.client.state(), b.client.state());

        // Nothing new: no downloads and no uploads.
        let downloads = remote.repo().downloads;
        assert_eq!(a.round(), MirrorReport::default());
        assert_eq!(b.round(), MirrorReport::default());
        assert_eq!(remote.repo().downloads, downloads);
        assert_eq!(remote.repo().files.len(), 3);
    }

    #[test]
    fn offline_changes_are_sent_when_the_network_is_back() {
        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        let mut b = Member::new("Bo", &remote);
        let id = a.submit(None, &create_action("Upright"));
        a.round();
        b.round();

        // The network goes away. Both take the request anyway.
        remote.repo().offline = true;
        a.submit(Some(id), &take_action("Anna"));
        b.submit(Some(id), &take_action("Bo"));
        let report = a.round();
        assert!(matches!(report.error, Some(RemoteError::Offline(_))));
        assert_eq!((report.uploaded, report.pending), (0, 1));
        assert_eq!(*a.client.status(), FolderStatus::Ok);
        assert_eq!(a.client.state().requests[&id].assignee, "Anna");
        assert_eq!(b.round().pending, 1);

        // The network comes back: everything is sent and both get the same winner.
        remote.repo().offline = false;
        assert_eq!(a.round().uploaded, 1);
        let report = b.round();
        assert_eq!(
            (report.downloaded, report.uploaded, report.pending),
            (1, 1, 0)
        );
        a.round();
        assert_eq!(a.client.state(), b.client.state());
        assert_eq!(a.client.state().requests[&id].status, Status::Running);
        let rejected = a.client.snapshot().rejected_ops;
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn busy_repo_is_retried() {
        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        a.round();

        a.submit(None, &create_action("Upright"));
        remote.repo().conflicts = 2;
        assert_eq!(a.round().uploaded, 1);

        a.submit(None, &create_action("Aero"));
        remote.repo().conflicts = 50;
        let report = a.round();
        assert_eq!(report.error, Some(RemoteError::Conflict));
        assert_eq!((report.uploaded, report.pending), (0, 1));
        remote.repo().conflicts = 0;
        let report = a.round();
        assert_eq!(
            (report.uploaded, report.pending, report.error),
            (1, 0, None)
        );
    }

    #[test]
    fn hostile_repo_content_is_ignored() {
        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        let id = a.submit(None, &create_action("Upright"));
        a.round();

        let good = remote
            .repo()
            .files
            .keys()
            .find(|k| k.starts_with("ops/"))
            .cloned()
            .unwrap();
        let name = good.rsplit('/').next().unwrap().to_string();
        let other = name.replace(&name[..13], "1794000000001");
        for path in [
            "../evil.json".to_string(),
            format!("ops/../{name}"),
            format!("ops/2026-11/../../{name}"),
            format!("ops/abcd-ef/{other}"),
            format!("ops/2026-11/sub/{other}"),
            format!("ops/{other}"),
            format!("/ops/2026-11/{other}"),
            "ops/2026-11/trasig.json".to_string(),
            "README.md".to_string(),
            ".github/workflows/x.yml".to_string(),
        ] {
            remote.put(&path, b"{\"v\": 1}");
        }
        // Valid name but too large a file, and valid name with junk inside.
        let big = name.replace(&name[..13], "1794000000002");
        remote.put(
            &format!("ops/2026-11/{big}"),
            &vec![b' '; MAX_FILE_BYTES as usize + 1],
        );
        let junk = name.replace(&name[..13], "1794000000003");
        remote.put(&format!("ops/2026-11/{junk}"), b"{ inte json");

        let mut b = Member::new("Bo", &remote);
        let report = b.round();
        assert_eq!(report.error, None);
        assert_eq!(report.downloaded, 2, "only the real file and the junk file");
        assert_eq!(b.client.state().requests[&id].title, "Upright");
        assert_eq!(b.client.state().requests.len(), 1);

        // Nothing ended up outside ops/, and the junk file is just waiting for retries.
        let mut top: Vec<String> = std::fs::read_dir(b.dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        assert_eq!(top, vec!["format.json".to_string(), "ops".to_string()]);
        assert_eq!(ops::scan_ops(b.dir.path()).unwrap().entries.len(), 2);
        let seen = b.client.store().seen();
        assert_eq!(
            seen.values()
                .filter(|s| s.state == SeenState::Retry)
                .count(),
            1
        );
        assert!(!b.dir.path().parent().unwrap().join("evil.json").exists());
    }

    #[test]
    fn newer_repo_format_blocks_writes_and_uploads() {
        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        a.round();
        a.submit(None, &create_action("Upright"));

        // Someone with a newer app has upgraded the repository.
        remote.put("format.json", b"{\"format_version\": 9}");
        let before = remote.repo().files.len();
        let report = a.round();
        assert_eq!((report.uploaded, report.pending), (0, 1));
        assert_eq!(remote.repo().files.len(), before);
        assert_eq!(*a.client.status(), FolderStatus::NewerFormat);
        assert!(a.client.submit(None, &create_action("Aero")).is_err());
    }

    #[test]
    fn uploaded_files_follow_the_request() {
        use crate::blobs::{self, FileRef};
        use crate::model::LocKind;
        use crate::ops::NewAction;
        use crate::sync::FileSlot;

        let remote = FakeRemote::default();
        let mut a = Member::new("Anna", &remote);
        let mut b = Member::new("Bo", &remote);
        a.round();
        let desk = TempDir::new();
        let cad = desk.path().join("Monokock åäö.step");
        let cad_bytes = vec![7u8; 300_000];
        std::fs::write(&cad, &cad_bytes).unwrap();

        // Anna creates a request with an uploaded CAD file as input.
        let mut create = create_action("Vridstyvhet");
        if let NewAction::Create { input, .. } = &mut create {
            *input = None;
        }
        let id =
            match a
                .client
                .submit_with_files(None, create, vec![(FileSlot::Input, cad.clone())])
            {
                Ok(Submitted::Applied { request_id, .. }) => request_id,
                other => panic!("unexpected: {other:?}"),
            };
        assert_eq!(blobs::local_files(a.dir.path()).len(), 1);
        let op_text = a.client.store().ops()[0].json.clone();
        assert!(op_text.contains("\"v\": 2"), "{op_text}");

        // The file is uploaded before the event, and the local copy is then removed.
        let report = a.round();
        assert_eq!(
            (report.files_uploaded, report.uploaded, report.pending_files),
            (1, 1, 0)
        );
        assert!(blobs::local_files(a.dir.path()).is_empty());
        assert_eq!(remote.repo().blobs.len(), 1);

        // Bo sees the request and can download exactly the same file.
        b.round();
        let input = b.client.state().requests[&id].input.clone().unwrap();
        assert_eq!(input.kind, LocKind::File);
        let file = FileRef::parse(&input.value).unwrap();
        assert_eq!(file.name, "Monokock åäö.step");
        assert_eq!(file.size, 300_000);
        let saved = desk.path().join("nedladdad.step");
        let progress = AtomicU64::new(0);
        blobs::fetch(b.dir.path(), Some(&mut b.remote), &file, &saved, &progress).unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), cad_bytes);
        assert_eq!(progress.load(Ordering::Relaxed), 300_000);

        // Bo finishes offline with an uploaded report and a Vault description.
        remote.repo().offline = true;
        let report_file = desk.path().join("Rapport.pdf");
        std::fs::write(&report_file, b"%PDF report").unwrap();
        b.submit(Some(id), &take_action("Bo"));
        let finish = NewAction::Finish {
            assignee: "Bo".to_string(),
            report: None,
            results: Some(crate::model::Location {
                kind: LocKind::Vault,
                value: "Chassi/Results".to_string(),
            }),
            summary: "Klart".to_string(),
        };
        match b
            .client
            .submit_with_files(Some(id), finish, vec![(FileSlot::Report, report_file)])
        {
            Ok(Submitted::Applied { .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
        let round = b.round();
        assert_eq!((round.pending_files, round.pending), (1, 2));
        // Until it is uploaded, others are told to wait.
        let report_ref =
            FileRef::parse(&b.client.state().requests[&id].report.clone().unwrap().value).unwrap();
        remote.repo().offline = false;
        let early = desk.path().join("för tidigt.pdf");
        assert!(blobs::fetch(
            a.dir.path(),
            Some(&mut a.remote),
            &report_ref,
            &early,
            &progress
        )
        .unwrap_err()
        .contains("uppladdad"));
        assert!(!early.exists());

        let round = b.round();
        assert_eq!(
            (
                round.files_uploaded,
                round.uploaded,
                round.pending_files,
                round.pending
            ),
            (1, 2, 0, 0)
        );
        a.round();
        assert_eq!(a.client.state().requests[&id].status, Status::Done);
        let got = desk.path().join("rapport.pdf");
        blobs::fetch(
            a.dir.path(),
            Some(&mut a.remote),
            &report_ref,
            &got,
            &progress,
        )
        .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"%PDF report");

        // A file that was tampered with in the store is refused.
        remote
            .repo()
            .blobs
            .insert(report_ref.sha256.clone(), b"%PDF forged".to_vec());
        let forged = desk.path().join("forged.pdf");
        assert!(blobs::fetch(
            a.dir.path(),
            Some(&mut a.remote),
            &report_ref,
            &forged,
            &progress
        )
        .is_err());
        assert!(!forged.exists());
    }

    #[test]
    fn team_keys() {
        assert_eq!(
            parse_team_key("  simq:lag-et/Sim.Q_repo:github_pat_ABC123def456  ").unwrap(),
            (
                "lag-et/Sim.Q_repo".to_string(),
                "github_pat_ABC123def456".to_string()
            )
        );
        assert!(parse_team_key("lag/repo:ghp_0123456789").is_ok());
        for bad in [
            "",
            "lag/repo",
            "lag/repo:",
            "lag/repo:kort",
            "repo:github_pat_ABC123def456",
            "lag/re po:github_pat_ABC123def456",
            "../x:github_pat_ABC123def456",
            "lag/..:github_pat_ABC123def456",
            "lag/repo/x:github_pat_ABC123def456",
            "lag/repo:github pat ABC123def456",
            "lag/repo:github_pat_åäö123456789",
        ] {
            assert!(parse_team_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn base64_and_paths() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode("åäö".as_bytes()), "w6XDpMO2");
        assert_eq!(encode("feature/sim q"), "feature%2Fsim%20q");
        assert!(safe_path("ops/2026-11/a_b-c.json"));
        assert!(!safe_path("ops/../x"));
        assert!(!safe_path("/etc/passwd"));
        assert!(!safe_path("ops/a b"));
        assert!(op_path("ops/2026-11/x.json").is_none());
    }

    // ----- Fake GitHub over HTTP --------------------------------------------
    //
    // Checks that the requests look the way the GitHub REST API expects
    // (URLs, headers, base64) and that the responses are parsed correctly.

    const TOKEN: &str = "github_pat_TESTNYCKEL123";

    #[derive(Default)]
    struct Server {
        files: BTreeMap<String, Vec<u8>>,
        version: u64,
        empty: bool,
        read_only: bool,
        requests: Vec<String>,
        base: String,
        releases: BTreeMap<String, u64>,
        /// (asset id, release id, name, content, state)
        assets: Vec<(u64, u64, String, Vec<u8>, String)>,
    }

    fn release_json(base: &str, id: u64) -> String {
        format!("{{\"id\":{id},\"upload_url\":\"{base}/upload/{id}{{?name,label}}\"}}")
    }

    fn base64_decode(text: &str) -> Vec<u8> {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0;
        for byte in text.bytes() {
            let Some(value) = TABLE.iter().position(|&c| c == byte) else {
                continue;
            };
            buffer = (buffer << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
            }
        }
        out
    }

    fn respond(stream: &mut TcpStream, code: u16, extra: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    fn handle(stream: &mut TcpStream, server: &Mutex<Server>) {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                return;
            }
            raw.extend_from_slice(&chunk[..n]);
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let header = |name: &str| {
            head.lines().skip(1).find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
        };
        let length: usize = header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while raw.len() < header_end + length {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
        }
        let body = raw[header_end..].to_vec();
        let mut parts = request_line.split(' ');
        let (method, target) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        let mut server = server.lock().unwrap();
        server.requests.push(format!("{method} {target}"));

        if header("authorization").as_deref() != Some(&format!("Bearer {TOKEN}")) {
            return respond(stream, 401, "", b"{}");
        }
        if header("user-agent").is_none() || header("x-github-api-version").is_none() {
            return respond(stream, 400, "", b"{}");
        }
        if let Some(upload) = target.strip_prefix("/upload/") {
            // Like uploads.github.com: needs a Content-Length, no chunked bodies.
            let (release, name) = upload.split_once("?name=").unwrap_or_default();
            if method != "POST"
                || header("transfer-encoding").is_some()
                || header("content-type").as_deref() != Some("application/octet-stream")
            {
                return respond(stream, 400, "", b"{}");
            }
            if server.assets.iter().any(|a| a.2 == name) {
                return respond(stream, 422, "", b"{}");
            }
            let id = 1000 + server.assets.len() as u64;
            let release: u64 = release.parse().unwrap_or(0);
            server
                .assets
                .push((id, release, name.to_string(), body, "uploaded".to_string()));
            return respond(stream, 201, "", b"{}");
        }
        let Some(rest) = target.strip_prefix("/repos/lag/simq") else {
            return respond(stream, 404, "", b"{}");
        };
        if let Some(tag) = rest.strip_prefix("/releases/tags/") {
            return match server.releases.get(tag) {
                Some(id) => respond(stream, 200, "", release_json(&server.base, *id).as_bytes()),
                None => respond(stream, 404, "", b"{}"),
            };
        }
        if method == "POST" && rest == "/releases" {
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let tag = json["tag_name"].as_str().unwrap_or_default().to_string();
            if json["prerelease"] != true || server.releases.contains_key(&tag) {
                return respond(stream, 422, "", b"{}");
            }
            let id = 1 + server.releases.len() as u64;
            server.releases.insert(tag, id);
            return respond(stream, 201, "", release_json(&server.base, id).as_bytes());
        }
        if let Some(asset) = rest.strip_prefix("/releases/assets/") {
            let id: u64 = asset.parse().unwrap_or(0);
            if method == "DELETE" {
                server.assets.retain(|a| a.0 != id);
                return respond(stream, 204, "", b"");
            }
            if !header("accept")
                .unwrap_or_default()
                .contains("octet-stream")
            {
                return respond(stream, 400, "", b"{}");
            }
            return match server.assets.iter().find(|a| a.0 == id) {
                Some(a) => respond(stream, 200, "", &a.3.clone()),
                None => respond(stream, 404, "", b"{}"),
            };
        }
        if let Some(listing) = rest.strip_prefix("/releases/") {
            if let Some((release, query)) = listing.split_once("/assets?") {
                let release: u64 = release.parse().unwrap_or(0);
                let items: Vec<String> = if query == "per_page=100&page=1" {
                    server
                        .assets
                        .iter()
                        .filter(|a| a.1 == release)
                        .map(|a| {
                            format!(
                                "{{\"id\":{},\"name\":\"{}\",\"url\":\"{}/repos/lag/simq/releases/assets/{}\",\"size\":{},\"state\":\"{}\"}}",
                                a.0, a.2, server.base, a.0, a.3.len(), a.4
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                return respond(stream, 200, "", format!("[{}]", items.join(",")).as_bytes());
            }
        }
        if method == "GET" && rest.is_empty() {
            let body = format!(
                "{{\"default_branch\":\"main\",\"permissions\":{{\"push\":{}}}}}",
                !server.read_only
            );
            return respond(stream, 200, "", body.as_bytes());
        }
        if method == "GET" && rest == "/git/trees/main?recursive=1" {
            if server.empty {
                return respond(
                    stream,
                    409,
                    "",
                    b"{\"message\":\"Git Repository is empty.\"}",
                );
            }
            let etag = format!("\"v{}\"", server.version);
            if header("if-none-match").as_deref() == Some(etag.as_str()) {
                return respond(stream, 304, "", b"");
            }
            let entries: Vec<String> = server
                .files
                .iter()
                .map(|(path, bytes)| {
                    format!(
                        "{{\"path\":\"{path}\",\"type\":\"blob\",\"size\":{},\"sha\":\"s{}\"}}",
                        bytes.len(),
                        bytes.len()
                    )
                })
                .chain(std::iter::once(
                    "{\"path\":\"ops\",\"type\":\"tree\",\"sha\":\"t\"}".to_string(),
                ))
                .collect();
            let body = format!(
                "{{\"sha\":\"x\",\"tree\":[{}],\"truncated\":false}}",
                entries.join(",")
            );
            return respond(stream, 200, &format!("ETag: {etag}\r\n"), body.as_bytes());
        }
        if let Some(path) = rest.strip_prefix("/contents/") {
            if method == "GET" {
                let Some(path) = path.strip_suffix("?ref=main") else {
                    return respond(stream, 400, "", b"{}");
                };
                if !header("accept").unwrap_or_default().contains("raw") {
                    return respond(stream, 400, "", b"{}");
                }
                return match server.files.get(path) {
                    Some(bytes) => respond(stream, 200, "", &bytes.clone()),
                    None => respond(stream, 404, "", b"{}"),
                };
            }
            if method == "PUT" {
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                if json["branch"] != "main" || !json["message"].is_string() {
                    return respond(stream, 400, "", b"{}");
                }
                if server.files.contains_key(path) {
                    return respond(stream, 422, "", b"{\"message\":\"sha wasn't supplied\"}");
                }
                let content = base64_decode(json["content"].as_str().unwrap_or_default());
                server.files.insert(path.to_string(), content);
                server.version += 1;
                server.empty = false;
                return respond(stream, 201, "", b"{}");
            }
        }
        respond(stream, 404, "", b"{}")
    }

    fn start_server() -> (String, Arc<Mutex<Server>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = Arc::new(Mutex::new(Server {
            base: base.clone(),
            ..Server::default()
        }));
        let shared = Arc::clone(&server);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                handle(&mut stream, &shared);
            }
        });
        (base, server)
    }

    fn remote_at(base: &str, token: &str) -> GithubRemote {
        GithubRemote::with_base(
            base,
            &GithubConfig {
                repo: "lag/simq".to_string(),
                token: token.to_string(),
                branch: "main".to_string(),
            },
        )
    }

    #[test]
    fn github_api_calls() {
        let (base, server) = start_server();

        // Checking the key.
        assert_eq!(
            GithubRemote::check_at(&base, "lag/simq", TOKEN),
            Ok("main".to_string())
        );
        assert_eq!(
            GithubRemote::check_at(&base, "lag/simq", "fel-nyckel-123"),
            Err(RemoteError::Denied)
        );
        assert_eq!(
            GithubRemote::check_at(&base, "lag/annat", TOKEN),
            Err(RemoteError::NotFound)
        );
        server.lock().unwrap().read_only = true;
        assert!(matches!(
            GithubRemote::check_at(&base, "lag/simq", TOKEN),
            Err(RemoteError::Other(_))
        ));
        server.lock().unwrap().read_only = false;

        // Empty repository.
        server.lock().unwrap().empty = true;
        let mut remote = remote_at(&base, TOKEN);
        assert_eq!(
            remote.list(None),
            Ok(Listing::Files {
                etag: None,
                files: Vec::new()
            })
        );

        // Upload, list, download.
        let path =
            "ops/2026-11/1794000000000-0000_abcdef01_00000000-0000-4000-8000-000000000001.json";
        let content = "{\"by\": \"Åsa Öberg\"}".as_bytes();
        assert_eq!(remote.upload(path, content), Ok(()));
        assert_eq!(remote.upload(path, content), Err(RemoteError::Exists));
        assert_eq!(server.lock().unwrap().files[path], content);

        let listing = remote.list(None).unwrap();
        let Listing::Files { etag, files } = listing else {
            panic!("expected a file listing");
        };
        assert_eq!(
            files,
            vec![RemoteFile {
                path: path.to_string(),
                size: content.len() as u64,
                sha: format!("s{}", content.len()),
            }]
        );
        assert_eq!(remote.list(etag.as_deref()), Ok(Listing::Unchanged));
        assert_eq!(remote.download(path).unwrap(), content);
        assert_eq!(
            remote.download("ops/2026-11/saknas.json"),
            Err(RemoteError::NotFound)
        );
        assert!(matches!(
            remote.download("../x"),
            Err(RemoteError::Other(_))
        ));

        // Wrong key and no contact.
        assert_eq!(
            remote_at(&base, "fel-nyckel-123").list(None),
            Err(RemoteError::Denied)
        );
        let mut dead = remote_at("http://127.0.0.1:1", TOKEN);
        assert!(matches!(dead.list(None), Err(RemoteError::Offline(_))));

        let requests = server.lock().unwrap().requests.clone();
        assert!(requests.contains(&format!("PUT /repos/lag/simq/contents/{path}")));
        assert!(requests.contains(&"GET /repos/lag/simq/git/trees/main?recursive=1".to_string()));
    }

    #[test]
    fn github_file_transfers() {
        let (base, server) = start_server();
        let mut remote = remote_at(&base, TOKEN);
        let tmp = TempDir::new();
        let content: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let source = tmp.path().join("källa.step");
        std::fs::write(&source, &content).unwrap();
        let (sha256, size) = blobs::sha256_of(&source).unwrap();
        let progress = AtomicU64::new(0);
        let dest = tmp.path().join("hämtad.step");

        // Nothing uploaded yet.
        assert_eq!(
            remote.download_file(&sha256, size, &dest, &progress),
            Err(RemoteError::FileMissing)
        );

        // Upload creates the release for the file's bucket and stores the file.
        assert_eq!(remote.upload_file(&sha256, &source), Ok(()));
        {
            let server = server.lock().unwrap();
            let tag = format!("simq-files-{}", &sha256[..2]);
            assert!(server.releases.contains_key(&tag));
            assert_eq!(server.assets.len(), 1);
            assert_eq!(server.assets[0].2, sha256);
            assert_eq!(server.assets[0].3, content);
        }
        // Uploading again does nothing.
        assert_eq!(remote.upload_file(&sha256, &source), Ok(()));
        assert_eq!(server.lock().unwrap().assets.len(), 1);

        assert_eq!(
            remote.download_file(&sha256, size, &dest, &progress),
            Ok(())
        );
        assert_eq!(std::fs::read(&dest).unwrap(), content);
        assert_eq!(progress.load(Ordering::Relaxed), size);

        // A file that is larger than the event says is cut off.
        assert!(matches!(
            remote.download_file(&sha256, size - 1, &dest, &progress),
            Err(RemoteError::Other(_))
        ));

        // The remains of an interrupted upload are replaced.
        server.lock().unwrap().assets[0].4 = "starter".to_string();
        assert_eq!(
            remote.download_file(&sha256, size, &dest, &progress),
            Err(RemoteError::FileMissing)
        );
        assert_eq!(remote.upload_file(&sha256, &source), Ok(()));
        {
            let server = server.lock().unwrap();
            assert_eq!(server.assets.len(), 1);
            assert_eq!(server.assets[0].4, "uploaded");
        }

        // Names that are not a SHA-256 never reach the API.
        assert!(matches!(
            remote.upload_file("../evil", &source),
            Err(RemoteError::Other(_))
        ));
        assert_eq!(
            remote_at(&base, "fel-nyckel-123").upload_file(&sha256, &source),
            Err(RemoteError::Denied)
        );
    }

    #[test]
    fn whole_flow_over_http_with_transport_thread() {
        let (base, server) = start_server();
        server.lock().unwrap().empty = true;
        let mut a = Member::new("Anna", &FakeRemote::default());
        let mut b = Member::new("Bo", &FakeRemote::default());
        let id = a.submit(None, &create_action("Upright"));

        // A's transport thread uploads, B's downloads.
        let (tx, rx) = mpsc::channel();
        let spawn = |dir: &TempDir, tx: Sender<(RemoteStatus, bool)>| {
            spawn_transport(
                Box::new(remote_at(&base, TOKEN)),
                "lag/simq".to_string(),
                dir.path().to_path_buf(),
                Box::new(move |status, new_files| {
                    let _ = tx.send((status, new_files));
                }),
            )
            .unwrap()
        };
        let transport_a = spawn(&a.dir, tx.clone());
        let (status, new_files) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(
            (status.state, status.pending, new_files),
            (RemoteState::Ok, 0, false)
        );
        assert!(status.last_ok_ms.is_some());
        assert_eq!(server.lock().unwrap().files.len(), 2);
        drop(transport_a);

        let (tx_b, rx_b) = mpsc::channel();
        let transport_b = spawn(&b.dir, tx_b);
        let (status, new_files) = rx_b.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!((status.state, new_files), (RemoteState::Ok, true));
        b.client.sync();
        assert_eq!(b.client.state().requests[&id].title, "Upright");

        // B takes the request and wakes the transport instead of waiting 15 s.
        b.submit(Some(id), &take_action("Bo"));
        transport_b.wake();
        let (status, _) = rx_b.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!((status.state, status.pending), (RemoteState::Ok, 0));
        assert_eq!(server.lock().unwrap().files.len(), 3);
        a.remote = FakeRemote::default();
        let mut real = remote_at(&base, TOKEN);
        let report = mirror_once(&mut real, a.dir.path(), &mut a.state);
        assert_eq!(report.downloaded, 1);
        a.client.sync();
        assert_eq!(a.client.state().requests[&id].assignee, "Bo");
    }
}
