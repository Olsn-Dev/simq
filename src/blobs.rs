//! Uploaded files (CAD models, reports, results).
//!
//! A file is identified by the SHA-256 of its content. Locally it is stored as
//! `<folder>/files/<sha256>`. An event refers to it with a location of kind
//! "file" whose value is `sha256:<hex>:<size>:<name>`.
//!
//! Files are untrusted like everything else that is shared: the app only
//! stores and copies them, it never opens or runs them, and every download is
//! checked against the size and digest in the event.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::github::RemoteStore;
use crate::i18n::{is_en, t};

/// Largest file that can be uploaded.
pub const MAX_FILE_SIZE: u64 = 500 * 1024 * 1024;
pub const FILES_DIR: &str = "files";
const MAX_NAME_CHARS: usize = 200;

/// Reference to an uploaded file, as stored in an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// Lower-case hex, 64 characters.
    pub sha256: String,
    pub size: u64,
    /// The original file name, for display and as the suggested name when saving.
    pub name: String,
}

pub fn valid_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Makes a file name safe to show and to suggest when saving: no path
/// separators or control characters, and not too long.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

impl FileRef {
    /// Parses `sha256:<hex>:<size>:<name>`. Anything else gives `None`.
    pub fn parse(value: &str) -> Option<FileRef> {
        let rest = value.strip_prefix("sha256:")?;
        let (sha256, rest) = rest.split_once(':')?;
        let (size, name) = rest.split_once(':')?;
        if !valid_sha256(sha256) || size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let size: u64 = size.parse().ok()?;
        if size == 0 || size > MAX_FILE_SIZE || name.is_empty() || safe_name(name) != name {
            return None;
        }
        Some(FileRef {
            sha256: sha256.to_string(),
            size,
            name: name.to_string(),
        })
    }

    pub fn to_value(&self) -> String {
        format!("sha256:{}:{}:{}", self.sha256, self.size, self.name)
    }

    /// "12.3 MB"
    pub fn size_label(&self) -> String {
        size_label(self.size)
    }
}

pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} kB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn too_large() -> String {
    t(
        "Filen är större än 500 MB och kan inte laddas upp.",
        "The file is larger than 500 MB and cannot be uploaded.",
    )
    .to_string()
}

pub fn local_path(folder: &Path, sha256: &str) -> PathBuf {
    folder.join(FILES_DIR).join(sha256)
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Copies `reader` to `writer`, hashing on the way. Stops with an error if
/// more than `limit` bytes arrive.
fn copy_hashing(
    mut reader: impl Read,
    mut writer: impl Write,
    limit: u64,
    progress: Option<&AtomicU64>,
) -> std::io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(std::io::Error::other("too large"));
        }
        hasher.update(&buffer[..n]);
        writer.write_all(&buffer[..n])?;
        if let Some(progress) = progress {
            progress.store(total, Ordering::Relaxed);
        }
    }
    writer.flush()?;
    Ok((hex(hasher.finalize()), total))
}

/// Takes a file from the user's computer into the local file store, where the
/// sync picks it up. Returns the reference to put in the event.
pub fn import(folder: &Path, source: &Path) -> Result<FileRef, String> {
    let cannot_read = |e: std::io::Error| {
        format!(
            "{}: {e}",
            t("Kunde inte läsa filen", "Could not read the file")
        )
    };
    let meta = fs::metadata(source).map_err(cannot_read)?;
    if !meta.is_file() {
        return Err(t("Det där är inte en fil.", "That is not a file.").to_string());
    }
    if meta.len() == 0 {
        return Err(t("Filen är tom.", "The file is empty.").to_string());
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(too_large());
    }
    let name = safe_name(
        &source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );

    let dir = folder.join(FILES_DIR);
    fs::create_dir_all(&dir).map_err(cannot_read)?;
    let part = dir.join(format!("import-{}.part", uuid::Uuid::new_v4()));
    let result = (|| {
        let reader = fs::File::open(source)?;
        let mut writer = fs::File::create(&part)?;
        let hashed = copy_hashing(reader, &mut writer, MAX_FILE_SIZE, None)?;
        writer.sync_all()?;
        Ok::<_, std::io::Error>(hashed)
    })();
    let (sha256, size) = match result {
        Ok(hashed) => hashed,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(cannot_read(e));
        }
    };
    let target = dir.join(&sha256);
    if target.exists() {
        // The same content is already stored.
        let _ = fs::remove_file(&part);
    } else if let Err(e) = fs::rename(&part, &target) {
        let _ = fs::remove_file(&part);
        return Err(cannot_read(e));
    }
    Ok(FileRef { sha256, size, name })
}

/// Local files waiting in the store, as (sha256, path).
pub fn local_files(folder: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(folder.join(FILES_DIR)) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            (is_file && valid_sha256(&name)).then(|| (name, entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// Saves an uploaded file to `dest` on the user's computer: from the local
/// store if it is there, otherwise from the remote store. The content is
/// always checked against the size and digest in the reference.
pub fn fetch(
    folder: &Path,
    remote: Option<&mut dyn RemoteStore>,
    file: &FileRef,
    dest: &Path,
    progress: &AtomicU64,
) -> Result<(), String> {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let cannot_save = |e: std::io::Error| {
        format!(
            "{}: {e}",
            t("Kunde inte spara filen", "Could not save the file")
        )
    };

    let local = local_path(folder, &file.sha256);
    let result: Result<(String, u64), String> = if local.is_file() {
        (|| {
            let reader = fs::File::open(&local)?;
            let writer = fs::File::create(&part)?;
            copy_hashing(reader, writer, file.size, Some(progress))
        })()
        .map_err(cannot_save)
    } else if let Some(remote) = remote {
        remote
            .download_file(&file.sha256, file.size, &part, progress)
            .map_err(|e| e.text())
            .and_then(|()| sha256_of(&part).map_err(cannot_save))
    } else {
        Err(t(
            "Filen har inte synkats till den här datorn än. Försök igen om en stund.",
            "The file has not synced to this computer yet. Try again in a moment.",
        )
        .to_string())
    };

    let checked = result.and_then(|(sha256, size)| {
        if sha256 == file.sha256 && size == file.size {
            Ok(())
        } else {
            Err(t(
                "Den hämtade filen stämmer inte med kontrollsumman och har kastats.",
                "The fetched file does not match its checksum and has been discarded.",
            )
            .to_string())
        }
    });
    match checked {
        Ok(()) => fs::rename(&part, dest).map_err(|e| {
            let _ = fs::remove_file(&part);
            cannot_save(e)
        }),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

/// SHA-256 and size of a file on disk.
pub fn sha256_of(path: &Path) -> std::io::Result<(String, u64)> {
    copy_hashing(fs::File::open(path)?, std::io::sink(), u64::MAX, None)
}

/// "Uploading 2 files" for the status bar.
pub fn pending_label(count: usize) -> String {
    if is_en() {
        format!("· uploading {count} file(s)")
    } else {
        format!("· laddar upp {count} fil(er)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn references() {
        let file = FileRef {
            sha256: ABC.to_string(),
            size: 3,
            name: "Monokock åäö v2.step".to_string(),
        };
        let value = file.to_value();
        assert_eq!(value, format!("sha256:{ABC}:3:Monokock åäö v2.step"));
        assert_eq!(FileRef::parse(&value), Some(file));

        for bad in [
            String::new(),
            "Uprights/v3/Assembly.iam".to_string(),
            format!("sha256:{ABC}:3"),
            format!("sha256:{ABC}:3:"),
            format!("sha256:{ABC}:0:a.step"),
            format!("sha256:{ABC}:-3:a.step"),
            format!("sha256:{ABC}:{}:a.step", MAX_FILE_SIZE + 1),
            format!("sha256:{}:3:a.step", ABC.to_uppercase()),
            format!("sha256:{}:3:a.step", &ABC[..63]),
            format!("sha256:{ABC}:3:../../a.step"),
            format!("sha256:{ABC}:3:a\nb.step"),
            format!("md5:{ABC}:3:a.step"),
        ] {
            assert_eq!(FileRef::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(safe_name("Assembly.iam"), "Assembly.iam");
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name("C:\\CAD\\del.step"), "C__CAD_del.step");
        assert_eq!(safe_name("a\nb"), "a_b");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(2048), "2 kB");
        assert_eq!(size_label(5 * 1024 * 1024 + 300 * 1024), "5.3 MB");
        assert_eq!(size_label(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn import_and_fetch_locally() {
        let folder = TempDir::new();
        let user = TempDir::new();
        let source = user.path().join("Rapport åäö.pdf");
        fs::write(&source, b"abc").unwrap();

        let file = import(folder.path(), &source).unwrap();
        assert_eq!(
            file,
            FileRef {
                sha256: ABC.to_string(),
                size: 3,
                name: "Rapport åäö.pdf".to_string()
            }
        );
        assert_eq!(fs::read(local_path(folder.path(), ABC)).unwrap(), b"abc");
        // Importing the same content again changes nothing.
        import(folder.path(), &source).unwrap();
        assert_eq!(local_files(folder.path()).len(), 1);
        assert_eq!(
            fs::read_dir(folder.path().join(FILES_DIR)).unwrap().count(),
            1,
            "no temporary files are left behind"
        );

        let dest = user.path().join("hämtad.pdf");
        let progress = AtomicU64::new(0);
        fetch(folder.path(), None, &file, &dest, &progress).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        assert_eq!(progress.load(Ordering::Relaxed), 3);

        // A file that is not here cannot be fetched without a remote store.
        let missing = FileRef {
            sha256: "0".repeat(64),
            size: 3,
            name: "x".to_string(),
        };
        let other = user.path().join("x");
        assert!(fetch(folder.path(), None, &missing, &other, &progress).is_err());
        assert!(!other.exists());

        // Content that does not match the reference is discarded.
        fs::write(local_path(folder.path(), &"1".repeat(64)), b"abd").unwrap();
        let tampered = FileRef {
            sha256: "1".repeat(64),
            size: 3,
            name: "x".to_string(),
        };
        assert!(fetch(folder.path(), None, &tampered, &other, &progress).is_err());
        assert!(!other.exists());
        assert_eq!(fs::read_dir(user.path()).unwrap().count(), 2);
    }

    #[test]
    fn empty_and_missing_sources_are_refused() {
        let folder = TempDir::new();
        let user = TempDir::new();
        let empty = user.path().join("tom.step");
        fs::write(&empty, b"").unwrap();
        assert!(import(folder.path(), &empty).is_err());
        assert!(import(folder.path(), &user.path().join("saknas.step")).is_err());
        assert!(import(folder.path(), user.path()).is_err());
        assert!(local_files(folder.path()).is_empty());
    }
}
