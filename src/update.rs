//! In-app update.
//!
//! Looks at the latest GitHub release of the app, downloads the binary for
//! this platform, replaces the running executable and restarts.
//!
//! The download is trusted because it comes from the project's own GitHub
//! releases over HTTPS. The size and (when GitHub provides one) the SHA-256
//! digest are checked so that a broken download is never installed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::i18n::{is_en, t};

/// The repository whose releases contain new versions of the app.
pub const UPDATE_REPO: &str = "Olsn-Dev/simq";
const API_BASE: &str = "https://api.github.com";
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// The version this binary was built as.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Name of the release file for this platform, if automatic update is supported.
pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("simq-macos-arm64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("simq-windows-x64.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("simq-linux-x64")
    } else {
        None
    }
}

/// `v1.2.3` or `1.2.3` → (1, 2, 3).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let text = text.trim().trim_start_matches('v');
    let mut parts = text.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// A newer version that can be downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    /// Without the leading `v`.
    pub version: String,
    /// API address of the file.
    url: String,
    size: u64,
    sha256: Option<String>,
    /// True if the release could only be read with the team key.
    with_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    Available(Available),
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Picks the file for this platform out of a release description.
fn pick(release_json: &str, current: &str, wanted: Option<&str>) -> Result<Check, String> {
    let release: Release = serde_json::from_str(release_json).map_err(|e| e.to_string())?;
    if !is_newer(&release.tag_name, current) {
        return Ok(Check::UpToDate);
    }
    let version = release.tag_name.trim_start_matches('v').to_string();
    let wanted = wanted.ok_or_else(|| {
        if is_en() {
            format!("Version {version} is available, but automatic update is not supported on this kind of computer. Download it from the Releases page.")
        } else {
            format!("Version {version} finns, men automatisk uppdatering stöds inte på den här sortens dator. Ladda ner den från Releases-sidan.")
        }
    })?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == wanted)
        .ok_or_else(|| {
            if is_en() {
                format!("Version {version} is available, but it has no file for automatic update. Download it from the Releases page.")
            } else {
                format!("Version {version} finns, men den saknar fil för automatisk uppdatering. Ladda ner den från Releases-sidan.")
            }
        })?;
    if asset.size == 0 || asset.size > MAX_DOWNLOAD_BYTES {
        return Err(t(
            "Uppdateringsfilen har en orimlig storlek.",
            "The update file has an unreasonable size.",
        )
        .to_string());
    }
    Ok(Check::Available(Available {
        version,
        url: asset.url,
        size: asset.size,
        sha256: asset
            .digest
            .and_then(|d| d.strip_prefix("sha256:").map(str::to_lowercase)),
        with_token: false,
    }))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(180))
        .build()
}

fn get(agent: &ureq::Agent, url: &str, accept: &str, token: Option<&str>) -> ureq::Request {
    let mut request = agent
        .get(url)
        .set("Accept", accept)
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "simq");
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    request
}

fn network_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(404, _) | ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => t(
            "Hittar inga versioner. Om appens repo är privat måste det göras publikt, eller så måste teamnyckeln få läsa det.",
            "Cannot find any versions. If the app's repository is private it must be made public, or the team key must be allowed to read it.",
        )
        .to_string(),
        ureq::Error::Status(code, _) => format!("{} (HTTP {code})", t("GitHub svarade med ett fel", "GitHub answered with an error")),
        ureq::Error::Transport(e) => format!("{}: {e}", t("Ingen kontakt med GitHub", "No contact with GitHub")),
    }
}

/// Asks GitHub for the latest release. `token` is only used if the releases
/// cannot be read without it (a private repository).
pub fn check(token: Option<&str>) -> Result<Check, String> {
    check_at(
        API_BASE,
        UPDATE_REPO,
        token,
        current_version(),
        asset_name(),
    )
}

pub fn check_at(
    base: &str,
    repo: &str,
    token: Option<&str>,
    current: &str,
    wanted: Option<&str>,
) -> Result<Check, String> {
    let agent = agent();
    let url = format!(
        "{}/repos/{repo}/releases/latest",
        base.trim_end_matches('/')
    );
    let accept = "application/vnd.github+json";
    let (response, with_token) = match get(&agent, &url, accept, None).call() {
        Ok(response) => (response, false),
        Err(ureq::Error::Status(401 | 403 | 404, _)) if token.is_some() => (
            get(&agent, &url, accept, token)
                .call()
                .map_err(network_error)?,
            true,
        ),
        Err(e) => return Err(network_error(e)),
    };
    let body = response.into_string().map_err(|e| e.to_string())?;
    Ok(match pick(&body, current, wanted)? {
        Check::Available(mut available) => {
            available.with_token = with_token;
            Check::Available(available)
        }
        Check::UpToDate => Check::UpToDate,
    })
}

/// Downloads the new binary and checks its size and digest.
pub fn download(available: &Available, token: Option<&str>) -> Result<Vec<u8>, String> {
    let agent = agent();
    let token = if available.with_token { token } else { None };
    let response = get(&agent, &available.url, "application/octet-stream", token)
        .call()
        .map_err(network_error)?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(available.size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            format!(
                "{}: {e}",
                t("Nedladdningen avbröts", "The download was interrupted")
            )
        })?;
    if bytes.len() as u64 != available.size {
        return Err(t(
            "Nedladdningen blev ofullständig. Försök igen.",
            "The download was incomplete. Try again.",
        )
        .to_string());
    }
    if let Some(expected) = &available.sha256 {
        let actual: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if &actual != expected {
            return Err(t(
                "Den nedladdade filen stämmer inte med kontrollsumman. Inget har ändrats.",
                "The downloaded file does not match its checksum. Nothing has been changed.",
            )
            .to_string());
        }
    }
    if !looks_like_program(&bytes) {
        return Err(t(
            "Den nedladdade filen är inte ett program för den här datorn. Inget har ändrats.",
            "The downloaded file is not a program for this computer. Nothing has been changed.",
        )
        .to_string());
    }
    Ok(bytes)
}

/// A quick sanity check so that e.g. an error page is never installed.
fn looks_like_program(bytes: &[u8]) -> bool {
    if cfg!(target_os = "windows") {
        bytes.starts_with(b"MZ")
    } else if cfg!(target_os = "macos") {
        bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]) || bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE])
    } else {
        bytes.starts_with(b"\x7fELF")
    }
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Replaces the program file at `exe` with `bytes`. The old file is moved
/// aside first, and moved back if anything goes wrong.
pub fn install_at(exe: &Path, bytes: &[u8]) -> Result<(), String> {
    let cannot_write = |e: std::io::Error| {
        format!(
            "{} ({e})",
            t(
                "Kunde inte skriva den nya versionen. Flytta appen till en mapp där du får skriva (t.ex. Program) och försök igen.",
                "Could not write the new version. Move the app to a folder you can write to (e.g. Applications) and try again.",
            )
        )
    };
    let mut new_name = exe.as_os_str().to_owned();
    new_name.push(".new");
    let new_path = PathBuf::from(new_name);
    std::fs::write(&new_path, bytes).map_err(cannot_write)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755))
            .map_err(cannot_write)?;
    }

    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new_path);
        return Err(cannot_write(e));
    }
    if let Err(e) = std::fs::rename(&new_path, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new_path);
        return Err(cannot_write(e));
    }
    // A running program can be deleted on macOS and Linux. On Windows the old
    // file is removed at the next start instead.
    if cfg!(unix) {
        let _ = std::fs::remove_file(&old);
    }
    resign_bundle(exe);
    Ok(())
}

/// On macOS the app bundle is signed ad hoc. After the program inside it has
/// been replaced, the bundle is signed again so that macOS keeps accepting it.
fn resign_bundle(exe: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let bundle = exe.ancestors().find(|dir| {
        dir.extension().is_some_and(|ext| ext == "app") && dir.join("Contents").is_dir()
    });
    if let Some(bundle) = bundle {
        let _ = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(bundle)
            .output();
    }
}

fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// Downloads and installs `available`. Returns the installed version.
pub fn download_and_install(available: &Available, token: Option<&str>) -> Result<String, String> {
    let bytes = download(available, token)?;
    install_at(&current_exe()?, &bytes)?;
    Ok(available.version.clone())
}

/// Starts the (new) program with the same arguments. The caller then closes this one.
pub fn restart() -> Result<(), String> {
    std::process::Command::new(current_exe()?)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Removes the previous version left behind by an update on Windows.
pub fn clean_up_after_update() {
    if let Ok(exe) = current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_lang, Lang};
    use crate::testutil::TempDir;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("v1.2.x"), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("nightly", "0.2.0"));
        assert!(parse_version(current_version()).is_some());
    }

    fn release(tag: &str, assets: &str) -> String {
        format!("{{\"tag_name\": \"{tag}\", \"assets\": [{assets}]}}")
    }

    const ASSETS: &str = r#"
        {"name": "simq-v0.3.0-macos-arm64.dmg", "url": "https://api/dmg", "size": 5},
        {"name": "simq-macos-arm64", "url": "https://api/mac", "size": 7,
         "digest": "sha256:ABCDEF"},
        {"name": "simq-linux-x64", "url": "https://api/linux", "size": 9}"#;

    #[test]
    fn picks_the_file_for_this_platform() {
        let json = release("v0.3.0", ASSETS);
        assert_eq!(
            pick(&json, "0.2.0", Some("simq-macos-arm64")),
            Ok(Check::Available(Available {
                version: "0.3.0".to_string(),
                url: "https://api/mac".to_string(),
                size: 7,
                sha256: Some("abcdef".to_string()),
                with_token: false,
            }))
        );
        assert_eq!(
            pick(&json, "0.3.0", Some("simq-macos-arm64")),
            Ok(Check::UpToDate)
        );
        assert_eq!(
            pick(&json, "0.4.0", Some("simq-macos-arm64")),
            Ok(Check::UpToDate)
        );

        // A newer version without a file for this platform is reported, not installed.
        let error = pick(&json, "0.2.0", Some("simq-windows-x64.exe")).unwrap_err();
        assert!(error.contains("0.3.0"), "{error}");
        let error = with_lang(Lang::En, || pick(&json, "0.2.0", None).unwrap_err());
        assert!(error.contains("not supported"), "{error}");

        assert!(pick("{ inte json", "0.2.0", Some("x")).is_err());
        assert!(pick(
            &release("v9.0.0", r#"{"name": "x", "url": "u", "size": 0}"#),
            "0.2.0",
            Some("x")
        )
        .is_err());
    }

    #[test]
    fn install_replaces_the_program_and_keeps_it_on_failure() {
        let tmp = TempDir::new();
        let exe = tmp.path().join("simq");
        std::fs::write(&exe, b"old version").unwrap();
        install_at(&exe, b"new version").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new version");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the new file must be executable");
            assert!(!old_path(&exe).exists());
        }
        let mut leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "simq" && name != "simq.old")
            .collect();
        leftovers.sort();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // A program that does not exist cannot be replaced, and nothing is left behind.
        let missing = tmp.path().join("missing").join("simq");
        assert!(install_at(&missing, b"x").is_err());
    }

    /// A tiny fake GitHub that serves one release and one file.
    fn serve(file: Vec<u8>, digest: String, private: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let file_url = format!("{base}/asset/1");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut chunk = [0u8; 2048];
                while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&raw).to_lowercase();
                let authorised = head.contains("authorization: bearer team-token-123");
                let (code, body): (u16, Vec<u8>) = if private && !authorised {
                    (404, b"{}".to_vec())
                } else if head.starts_with("get /repos/lag/simq/releases/latest ") {
                    let name = asset_name().unwrap_or("simq-test");
                    (
                        200,
                        format!(
                            "{{\"tag_name\":\"v9.9.9\",\"assets\":[{{\"name\":\"{name}\",\"url\":\"{file_url}\",\"size\":{},\"digest\":\"sha256:{digest}\"}}]}}",
                            file.len()
                        )
                        .into_bytes(),
                    )
                } else if head.starts_with("get /asset/1 ")
                    && head.contains("accept: application/octet-stream")
                {
                    (200, file.clone())
                } else {
                    (400, b"{}".to_vec())
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(&body);
            }
        });
        base
    }

    fn program_bytes() -> Vec<u8> {
        let mut bytes = if cfg!(target_os = "windows") {
            b"MZ".to_vec()
        } else if cfg!(target_os = "macos") {
            vec![0xCF, 0xFA, 0xED, 0xFE]
        } else {
            b"\x7fELF".to_vec()
        };
        bytes.extend_from_slice(b" pretend program");
        bytes
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn check_and_download_from_a_fake_github() {
        let file = program_bytes();
        let wanted = Some(asset_name().unwrap_or("simq-test"));

        // Public repository: no key needed.
        let base = serve(file.clone(), sha256_hex(&file), false);
        let Check::Available(available) =
            check_at(&base, "lag/simq", None, "0.2.0", wanted).unwrap()
        else {
            panic!("expected an available version");
        };
        assert_eq!(available.version, "9.9.9");
        assert!(!available.with_token);
        assert_eq!(download(&available, None).unwrap(), file);
        assert_eq!(
            check_at(&base, "lag/simq", None, "9.9.9", wanted),
            Ok(Check::UpToDate)
        );

        // Private repository: works only with a key that may read it.
        let base = serve(file.clone(), sha256_hex(&file), true);
        assert!(check_at(&base, "lag/simq", None, "0.2.0", wanted).is_err());
        assert!(check_at(&base, "lag/simq", Some("wrong-token"), "0.2.0", wanted).is_err());
        let Check::Available(available) =
            check_at(&base, "lag/simq", Some("team-token-123"), "0.2.0", wanted).unwrap()
        else {
            panic!("expected an available version");
        };
        assert!(available.with_token);
        assert_eq!(download(&available, Some("team-token-123")).unwrap(), file);
        assert!(download(&available, None).is_err());

        // A file that does not match its checksum is refused.
        let base = serve(file.clone(), sha256_hex(b"something else"), false);
        let Check::Available(available) =
            check_at(&base, "lag/simq", None, "0.2.0", wanted).unwrap()
        else {
            panic!("expected an available version");
        };
        let error = download(&available, None).unwrap_err();
        assert!(error.contains("kontrollsumman"), "{error}");

        // So is something that is not a program.
        let junk = b"<html>error</html>".to_vec();
        let base = serve(junk.clone(), sha256_hex(&junk), false);
        let Check::Available(available) =
            check_at(&base, "lag/simq", None, "0.2.0", wanted).unwrap()
        else {
            panic!("expected an available version");
        };
        assert!(download(&available, None).is_err());
    }
}
