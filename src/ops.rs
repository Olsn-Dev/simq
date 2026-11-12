//! The event format and the files in the shared folder.
//!
//! Everything read from the folder is untrusted input: the size is limited,
//! JSON is parsed strictly and all fields are validated before they are used.
//! Existing files are never overwritten and never deleted.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::hlc::Hlc;
use crate::i18n::{is_en, t};
use crate::model::{
    char_len, parse_location, truncate_chars, valid_deadline, LocKind, Location, OpKind, MAX_NAME,
    MAX_TEXT, MAX_TITLE,
};

/// Largest allowed size of an event file.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Highest event version this app understands.
pub const SUPPORTED_V: u32 = 2;
/// Events that refer to uploaded files use this version, so that apps too old
/// to handle files ignore them and ask for an update instead of rejecting them.
pub const FILES_V: u32 = 2;
/// Highest folder format this app may write to.
pub const FORMAT_VERSION: u32 = 1;

pub const OPS_DIR: &str = "ops";
pub const FORMAT_FILE: &str = "format.json";

/// Sort key for the total order.
pub type OpKey = (u64, u32, Uuid, Uuid);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePayload {
    pub title: String,
    pub description: String,
    pub sim_type: String,
    pub priority: u8,
    pub requester: String,
    pub deadline: String,
    pub input: Option<Location>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishPayload {
    pub assignee: String,
    pub report: Location,
    pub results: Option<Location>,
    pub summary: String,
}

/// The parsed and validated effect of an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create(Box<CreatePayload>),
    Take {
        assignee: String,
    },
    Finish(Box<FinishPayload>),
    Fail {
        note: String,
    },
    Cancel {
        note: String,
    },
    Requeue,
    Delete {
        note: String,
    },
    /// Unknown `kind` or a higher `v` than the app knows: stored but has no effect.
    Unsupported,
    /// Known `kind` but invalid content: rejected at replay with this reason.
    Invalid(String),
}

/// An event as it is stored in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub v: u32,
    pub op_id: Uuid,
    pub client_id: Uuid,
    pub hlc: Hlc,
    pub by: String,
    pub kind: String,
    pub request_id: Uuid,
    pub action: Action,
    /// The file's text, stored unchanged in the cache.
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// The file may be half-synced. Try again next round.
    #[error("{0}")]
    Incomplete(String),
    /// The file is complete but invalid. It is ignored permanently.
    #[error("{0}")]
    Invalid(String),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOp {
    v: u32,
    op_id: Uuid,
    client_id: Uuid,
    hlc: Hlc,
    by: String,
    kind: String,
    request_id: Uuid,
    payload: Value,
}

/// Events from newer app versions: only what is needed for ordering and deduplication.
#[derive(Deserialize)]
struct LenientOp {
    op_id: Uuid,
    client_id: Uuid,
    hlc: Hlc,
    request_id: Uuid,
    #[serde(default)]
    by: String,
    #[serde(default)]
    kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWire {
    title: String,
    description: String,
    sim_type: String,
    priority: u8,
    requester: String,
    deadline: String,
    input_kind: String,
    input_ref: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TakeWire {
    assignee: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishWire {
    assignee: String,
    report_kind: String,
    report_ref: String,
    results_kind: String,
    results_ref: String,
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteWire {
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyWire {}

fn check_text(field: &str, value: &str, max: usize, required: bool) -> Result<(), String> {
    if char_len(value) > max {
        return Err(if is_en() {
            format!("{field} is too long (max {max} characters)")
        } else {
            format!("{field} är för lång (max {max} tecken)")
        });
    }
    if required && value.trim().is_empty() {
        return Err(format!("{field} {}", t("saknas", "is missing")));
    }
    Ok(())
}

fn validate_create(w: CreateWire) -> Result<CreatePayload, String> {
    check_text(t("titel", "title"), &w.title, MAX_TITLE, true)?;
    check_text(
        t("beskrivning", "description"),
        &w.description,
        MAX_TEXT,
        false,
    )?;
    check_text(t("typ", "type"), &w.sim_type, MAX_NAME, false)?;
    check_text(t("beställare", "requester"), &w.requester, MAX_NAME, true)?;
    if !(1..=3).contains(&w.priority) {
        return Err(t(
            "prioritet måste vara 1, 2 eller 3",
            "priority must be 1, 2 or 3",
        )
        .to_string());
    }
    if !valid_deadline(&w.deadline) {
        return Err(t(
            "deadline måste vara tom eller ÅÅÅÅ-MM-DD",
            "deadline must be empty or YYYY-MM-DD",
        )
        .to_string());
    }
    let input = parse_location(&w.input_kind, &w.input_ref)
        .map_err(|e| format!("{}: {e}", t("indataplats", "input location")))?;
    Ok(CreatePayload {
        title: w.title,
        description: w.description,
        sim_type: w.sim_type,
        priority: w.priority,
        requester: w.requester,
        deadline: w.deadline,
        input,
    })
}

fn validate_finish(w: FinishWire) -> Result<FinishPayload, String> {
    check_text(t("utförare", "assignee"), &w.assignee, MAX_NAME, true)?;
    check_text(t("sammanfattning", "summary"), &w.summary, MAX_TEXT, true)?;
    let report = parse_location(&w.report_kind, &w.report_ref)
        .map_err(|e| format!("{}: {e}", t("rapportplats", "report location")))?
        .ok_or_else(|| t("rapportplats saknas", "report location is missing").to_string())?;
    let results = parse_location(&w.results_kind, &w.results_ref)
        .map_err(|e| format!("{}: {e}", t("resultatplats", "results location")))?;
    Ok(FinishPayload {
        assignee: w.assignee,
        report,
        results,
        summary: w.summary,
    })
}

fn from_payload<T: serde::de::DeserializeOwned>(payload: Value) -> Result<T, String> {
    serde_json::from_value(payload)
        .map_err(|e| format!("{}: {e}", t("felaktigt innehåll", "malformed content")))
}

fn parse_action(kind: &str, payload: Value) -> Action {
    let Some(kind) = OpKind::parse(kind) else {
        return Action::Unsupported;
    };
    let result = match kind {
        OpKind::Create => from_payload::<CreateWire>(payload)
            .and_then(validate_create)
            .map(|p| Action::Create(Box::new(p))),
        OpKind::Take => from_payload::<TakeWire>(payload).and_then(|w| {
            check_text(t("utförare", "assignee"), &w.assignee, MAX_NAME, true)?;
            Ok(Action::Take {
                assignee: w.assignee,
            })
        }),
        OpKind::Finish => from_payload::<FinishWire>(payload)
            .and_then(validate_finish)
            .map(|p| Action::Finish(Box::new(p))),
        OpKind::Fail => from_payload::<NoteWire>(payload).and_then(|w| {
            check_text(t("anteckning", "note"), &w.note, MAX_TEXT, false)?;
            Ok(Action::Fail { note: w.note })
        }),
        OpKind::Cancel => from_payload::<NoteWire>(payload).and_then(|w| {
            check_text(t("anteckning", "note"), &w.note, MAX_TEXT, false)?;
            Ok(Action::Cancel { note: w.note })
        }),
        OpKind::Requeue => from_payload::<EmptyWire>(payload).map(|_| Action::Requeue),
        OpKind::Delete => from_payload::<NoteWire>(payload).and_then(|w| {
            check_text(t("anteckning", "note"), &w.note, MAX_TEXT, false)?;
            Ok(Action::Delete { note: w.note })
        }),
    };
    result.unwrap_or_else(|e| Action::Invalid(truncate_chars(&e, 300)))
}

impl Op {
    /// Parses the content of an event file.
    pub fn parse(text: &str) -> Result<Op, ReadError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| ReadError::Incomplete(e.to_string()))?;
        let v = value.get("v").and_then(Value::as_u64).ok_or_else(|| {
            ReadError::Invalid(t("fältet v saknas", "the field v is missing").to_string())
        })?;
        let v = u32::try_from(v).ok().filter(|v| *v >= 1).ok_or_else(|| {
            ReadError::Invalid(t("ogiltig version", "invalid version").to_string())
        })?;

        if v > SUPPORTED_V {
            let w: LenientOp =
                serde_json::from_value(value).map_err(|e| ReadError::Invalid(e.to_string()))?;
            return Ok(Op {
                v,
                op_id: w.op_id,
                client_id: w.client_id,
                hlc: w.hlc,
                by: truncate_chars(&w.by, MAX_NAME),
                kind: truncate_chars(&w.kind, 64),
                request_id: w.request_id,
                action: Action::Unsupported,
                json: text.to_string(),
            });
        }

        let w: WireOp =
            serde_json::from_value(value).map_err(|e| ReadError::Invalid(e.to_string()))?;
        if char_len(&w.by) > MAX_NAME {
            return Err(ReadError::Invalid(
                t("fältet by är för långt", "the field by is too long").to_string(),
            ));
        }
        if char_len(&w.kind) > 64 {
            return Err(ReadError::Invalid(
                t("fältet kind är för långt", "the field kind is too long").to_string(),
            ));
        }
        let mut action = parse_action(&w.kind, w.payload);
        if v < FILES_V && action.uses_file() {
            action = Action::Invalid(
                t(
                    "uppladdade filer kräver en nyare händelseversion",
                    "uploaded files require a newer event version",
                )
                .to_string(),
            );
        }
        Ok(Op {
            v,
            op_id: w.op_id,
            client_id: w.client_id,
            hlc: w.hlc,
            by: w.by,
            kind: w.kind,
            request_id: w.request_id,
            action,
            json: text.to_string(),
        })
    }

    /// Total order for replay: (wall_ms, counter, client_id, op_id).
    pub fn key(&self) -> OpKey {
        (
            self.hlc.wall_ms,
            self.hlc.counter,
            self.client_id,
            self.op_id,
        )
    }

    pub fn op_kind(&self) -> Option<OpKind> {
        if self.v > SUPPORTED_V {
            None
        } else {
            OpKind::parse(&self.kind)
        }
    }
}

/// An action the user wants to perform, before it has become an event.
#[derive(Debug, Clone)]
pub enum NewAction {
    Create {
        title: String,
        description: String,
        sim_type: String,
        priority: u8,
        requester: String,
        deadline: String,
        input: Option<Location>,
    },
    Take {
        assignee: String,
    },
    Finish {
        assignee: String,
        report: Option<Location>,
        results: Option<Location>,
        summary: String,
    },
    Fail {
        note: String,
    },
    Cancel {
        note: String,
    },
    Requeue,
    /// Hides the request for everyone. Cannot be undone.
    Delete {
        note: String,
    },
}

fn loc_wire(loc: &Option<Location>) -> (&str, &str) {
    match loc {
        Some(l) => (l.kind.as_str(), l.value.as_str()),
        None => ("", ""),
    }
}

fn is_file(location: &Option<Location>) -> bool {
    matches!(location, Some(l) if l.kind == LocKind::File)
}

impl Action {
    /// True if the event refers to an uploaded file.
    pub fn uses_file(&self) -> bool {
        match self {
            Action::Create(p) => is_file(&p.input),
            Action::Finish(p) => p.report.kind == LocKind::File || is_file(&p.results),
            _ => false,
        }
    }
}

impl NewAction {
    /// True if the action refers to an uploaded file.
    pub fn uses_file(&self) -> bool {
        match self {
            NewAction::Create { input, .. } => is_file(input),
            NewAction::Finish {
                report, results, ..
            } => is_file(report) || is_file(results),
            _ => false,
        }
    }

    pub fn kind(&self) -> OpKind {
        match self {
            NewAction::Create { .. } => OpKind::Create,
            NewAction::Take { .. } => OpKind::Take,
            NewAction::Finish { .. } => OpKind::Finish,
            NewAction::Fail { .. } => OpKind::Fail,
            NewAction::Cancel { .. } => OpKind::Cancel,
            NewAction::Requeue => OpKind::Requeue,
            NewAction::Delete { .. } => OpKind::Delete,
        }
    }

    fn payload(&self) -> Value {
        match self {
            NewAction::Create {
                title,
                description,
                sim_type,
                priority,
                requester,
                deadline,
                input,
            } => {
                let (input_kind, input_ref) = loc_wire(input);
                json!({
                    "title": title,
                    "description": description,
                    "sim_type": sim_type,
                    "priority": priority,
                    "requester": requester,
                    "deadline": deadline,
                    "input_kind": input_kind,
                    "input_ref": input_ref,
                })
            }
            NewAction::Take { assignee } => json!({ "assignee": assignee }),
            NewAction::Finish {
                assignee,
                report,
                results,
                summary,
            } => {
                let (report_kind, report_ref) = loc_wire(report);
                let (results_kind, results_ref) = loc_wire(results);
                json!({
                    "assignee": assignee,
                    "report_kind": report_kind,
                    "report_ref": report_ref,
                    "results_kind": results_kind,
                    "results_ref": results_ref,
                    "summary": summary,
                })
            }
            NewAction::Fail { note } | NewAction::Cancel { note } | NewAction::Delete { note } => {
                json!({ "note": note })
            }
            NewAction::Requeue => json!({}),
        }
    }
}

/// Builds a new event. It goes through exactly the same parsing and validation
/// as foreign files, so what is written can always be read back.
pub fn build_op(
    client_id: Uuid,
    hlc: Hlc,
    by: &str,
    request_id: Uuid,
    action: &NewAction,
) -> Result<Op, String> {
    let wire = WireOp {
        v: if action.uses_file() { FILES_V } else { 1 },
        op_id: Uuid::new_v4(),
        client_id,
        hlc,
        by: by.to_string(),
        kind: action.kind().as_str().to_string(),
        request_id,
        payload: action.payload(),
    };
    let text = serde_json::to_string_pretty(&wire).map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(t("händelsen är för stor", "the event is too large").to_string());
    }
    Op::parse(&text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------

/// `{wall_ms:013}-{counter:04}_{first_8_of_client_id}_{op_id}.json`
pub fn file_name(op: &Op) -> String {
    let client = op.client_id.to_string();
    format!(
        "{:013}-{:04}_{}_{}.json",
        op.hlc.wall_ms,
        op.hlc.counter,
        &client[..8],
        op.op_id
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub wall_ms: u64,
    pub counter: u32,
    pub client_prefix: String,
    pub op_id: Uuid,
}

/// Only accepts names that match the pattern exactly. Anything else (.DS_Store,
/// desktop.ini, *.part, "name (1).json" …) gives `None`.
pub fn parse_file_name(name: &str) -> Option<ParsedName> {
    let stem = name.strip_suffix(".json")?;
    if stem.len() != 13 + 1 + 4 + 1 + 8 + 1 + 36 || !stem.is_ascii() {
        return None;
    }
    let (wall, rest) = stem.split_at(13);
    let rest = rest.strip_prefix('-')?;
    let (counter, rest) = rest.split_at(4);
    let rest = rest.strip_prefix('_')?;
    let (client, rest) = rest.split_at(8);
    let op_id = rest.strip_prefix('_')?;

    if !wall.bytes().all(|b| b.is_ascii_digit()) || !counter.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !client
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let parsed = Uuid::parse_str(op_id).ok()?;
    if parsed.to_string() != op_id {
        return None;
    }
    Some(ParsedName {
        wall_ms: wall.parse().ok()?,
        counter: counter.parse().ok()?,
        client_prefix: client.to_string(),
        op_id: parsed,
    })
}

/// Subfolder `YYYY-MM` (UTC) for an event.
pub fn month_dir(wall_ms: u64) -> String {
    i64::try_from(wall_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.format("%Y-%m").to_string())
        .unwrap_or_else(|| "0000-00".to_string())
}

// ---------------------------------------------------------------------------
// Folder layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderError {
    #[error("{}", t("Mappen hittas inte", "The folder cannot be found"))]
    NotFound,
    #[error(
        "{} {0}{}",
        t(
            "Mappen använder ett nyare format (version",
            "The folder uses a newer format (version"
        ),
        t("). Uppdatera appen.", "). Update the app.")
    )]
    NewerFormat(u32),
    #[error(
        "{}: {0}",
        t("format.json kan inte läsas ännu", "format.json cannot be read yet")
    )]
    BadFormat(String),
    #[error(
        "{}: {0}",
        t("Det går inte att skriva i mappen", "Cannot write to the folder")
    )]
    Io(String),
}

#[derive(Serialize, Deserialize)]
struct FormatFile {
    format_version: u32,
}

/// Checks the folder and creates `ops/` and `format.json` if they are missing.
/// Gives `NewerFormat` if the folder requires a newer app (then nothing may be written).
pub fn ensure_layout(folder: &Path) -> Result<(), FolderError> {
    if !folder.is_dir() {
        return Err(FolderError::NotFound);
    }
    let format_path = folder.join(FORMAT_FILE);
    match read_limited(&format_path) {
        Ok(bytes) => {
            let parsed: FormatFile = serde_json::from_slice(&bytes)
                .map_err(|e| FolderError::BadFormat(e.to_string()))?;
            if parsed.format_version > FORMAT_VERSION {
                return Err(FolderError::NewerFormat(parsed.format_version));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let text = serde_json::to_string(&FormatFile {
                format_version: FORMAT_VERSION,
            })
            .map_err(|e| FolderError::Io(e.to_string()))?;
            write_new_file(&format_path, text.as_bytes())
                .map_err(|e| FolderError::Io(e.to_string()))?;
        }
        Err(e) => return Err(FolderError::BadFormat(e.to_string())),
    }
    let ops_dir = folder.join(OPS_DIR);
    if !ops_dir.is_dir() {
        fs::create_dir_all(&ops_dir).map_err(|e| FolderError::Io(e.to_string()))?;
    }
    Ok(())
}

fn read_limited(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Writes a NEW file: first `<name>.part`, flush + fsync, then rename.
/// An existing file is never overwritten.
pub(crate) fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            t("filen finns redan", "the file already exists"),
        ));
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = (|| {
        let mut file = fs::File::create(&part)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&part, path)
    })();
    if result.is_err() {
        // Only clean up our own temporary file.
        let _ = fs::remove_file(&part);
    }
    result
}

/// Writes an event to `<folder>/ops/YYYY-MM/<file name>.json`.
/// Returns the relative path below `ops/`.
pub fn write_op(folder: &Path, op: &Op) -> std::io::Result<String> {
    let month = month_dir(op.hlc.wall_ms);
    let name = file_name(op);
    let dir = folder.join(OPS_DIR).join(&month);
    fs::create_dir_all(&dir)?;
    write_new_file(&dir.join(&name), op.json.as_bytes())?;
    Ok(format!("{month}/{name}"))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    /// Relative path below `ops/`, e.g. `2026-11/<file name>.json`.
    pub rel: String,
    pub dir: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub entries: Vec<ScanEntry>,
    /// Files that do not match the name pattern (relative paths).
    pub ignored: Vec<String>,
    pub errors: Vec<String>,
}

/// Lists `ops/**/*.json`. Only files in a month folder with a name that
/// matches the pattern are included, everything else ends up in `ignored`.
pub fn scan_ops(folder: &Path) -> std::io::Result<Scan> {
    let root = folder.join(OPS_DIR);
    let mut scan = Scan::default();
    for entry in fs::read_dir(&root)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                scan.errors.push(e.to_string());
                continue;
            }
        };
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            scan.ignored.push(dir_name);
            continue;
        }
        let sub = match fs::read_dir(entry.path()) {
            Ok(s) => s,
            Err(e) => {
                scan.errors.push(format!("{dir_name}: {e}"));
                continue;
            }
        };
        for file in sub {
            let file = match file {
                Ok(f) => f,
                Err(e) => {
                    scan.errors.push(format!("{dir_name}: {e}"));
                    continue;
                }
            };
            let name = file.file_name().to_string_lossy().into_owned();
            let is_file = file.file_type().map(|t| t.is_file()).unwrap_or(false);
            if is_file && parse_file_name(&name).is_some() {
                scan.entries.push(ScanEntry {
                    rel: format!("{dir_name}/{name}"),
                    dir: dir_name.clone(),
                    name,
                    path: file.path(),
                });
            } else {
                scan.ignored.push(format!("{dir_name}/{name}"));
            }
        }
    }
    scan.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    scan.ignored.sort();
    Ok(scan)
}

/// Reads and validates an event file. File name and month folder must agree
/// with the content, so that an event can only exist in one single place.
pub fn read_op_file(entry: &ScanEntry) -> Result<Op, ReadError> {
    let meta = fs::metadata(&entry.path).map_err(|e| ReadError::Incomplete(e.to_string()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(ReadError::Invalid(
            t(
                "filen är större än 64 KiB",
                "the file is larger than 64 KiB",
            )
            .to_string(),
        ));
    }
    let bytes = read_limited(&entry.path).map_err(|e| ReadError::Incomplete(e.to_string()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ReadError::Invalid(
            t(
                "filen är större än 64 KiB",
                "the file is larger than 64 KiB",
            )
            .to_string(),
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| ReadError::Incomplete(format!("{}: {e}", t("inte UTF-8", "not UTF-8"))))?;
    if text.trim().is_empty() {
        return Err(ReadError::Incomplete(
            t("filen är tom", "the file is empty").to_string(),
        ));
    }
    let op = Op::parse(text)?;
    if file_name(&op) != entry.name {
        return Err(ReadError::Invalid(
            t(
                "filnamnet stämmer inte med innehållet",
                "the file name does not match the content",
            )
            .to_string(),
        ));
    }
    if month_dir(op.hlc.wall_ms) != entry.dir {
        return Err(ReadError::Invalid(
            t(
                "filen ligger i fel månadsmapp",
                "the file is in the wrong month folder",
            )
            .to_string(),
        ));
    }
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LocKind;
    use crate::testutil::{create_action, TempDir};

    fn hlc(wall_ms: u64, counter: u32) -> Hlc {
        Hlc { wall_ms, counter }
    }

    fn sample_op() -> Op {
        build_op(
            Uuid::new_v4(),
            hlc(1_794_000_000_000, 3),
            "Åsa Öberg",
            Uuid::new_v4(),
            &create_action("Styvhet bakre upright"),
        )
        .expect("valid event")
    }

    /// Puts a file with the given content where `op` would be stored.
    fn plant(folder: &Path, op: &Op, content: &[u8]) -> ScanEntry {
        let dir = month_dir(op.hlc.wall_ms);
        let name = file_name(op);
        let path = folder.join(OPS_DIR).join(&dir);
        fs::create_dir_all(&path).unwrap();
        let path = path.join(&name);
        fs::write(&path, content).unwrap();
        ScanEntry {
            rel: format!("{dir}/{name}"),
            dir,
            name,
            path,
        }
    }

    #[test]
    fn file_name_round_trip() {
        let op = sample_op();
        let name = file_name(&op);
        assert!(name.starts_with("1794000000000-0003_"));
        let parsed = parse_file_name(&name).expect("the name should match the pattern");
        assert_eq!(parsed.wall_ms, op.hlc.wall_ms);
        assert_eq!(parsed.counter, 3);
        assert_eq!(parsed.op_id, op.op_id);
        assert_eq!(parsed.client_prefix, op.client_id.to_string()[..8]);
    }

    #[test]
    fn wrong_file_names_are_ignored() {
        let good = file_name(&sample_op());
        let stem = good.strip_suffix(".json").unwrap();
        let bad = [
            ".DS_Store".to_string(),
            "desktop.ini".to_string(),
            "format.json".to_string(),
            format!("{stem}.json.part"),
            format!("{stem} (1).json"),
            format!("{stem}.JSON"),
            format!("{stem}.txt"),
            format!("x{}", &good[1..]),
            good.replace('_', "-"),
            good.to_uppercase(),
            format!("../{good}"),
            String::new(),
            "åäö.json".to_string(),
        ];
        for name in &bad {
            assert!(parse_file_name(name).is_none(), "{name} should be ignored");
        }
        assert!(parse_file_name(&good).is_some());
    }

    #[test]
    fn write_then_read_back() {
        let tmp = TempDir::new();
        ensure_layout(tmp.path()).unwrap();
        let op = sample_op();
        let rel = write_op(tmp.path(), &op).unwrap();
        assert_eq!(rel, format!("2026-11/{}", file_name(&op)));

        let scan = scan_ops(tmp.path()).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert!(scan.ignored.is_empty());
        let read = read_op_file(&scan.entries[0]).unwrap();
        assert_eq!(read, op);
        assert_eq!(read.by, "Åsa Öberg");

        // No .part file is left behind and the same event cannot be written twice.
        assert!(write_op(tmp.path(), &op).is_err());
        assert_eq!(scan_ops(tmp.path()).unwrap().entries.len(), 1);
    }

    #[test]
    fn scan_ignores_everything_that_is_not_an_op_file() {
        let tmp = TempDir::new();
        ensure_layout(tmp.path()).unwrap();
        let op = sample_op();
        write_op(tmp.path(), &op).unwrap();
        let month = tmp.path().join(OPS_DIR).join("2026-11");
        let good = file_name(&op);
        let stem = good.strip_suffix(".json").unwrap();
        fs::write(month.join(".DS_Store"), b"x").unwrap();
        fs::write(month.join("desktop.ini"), b"x").unwrap();
        fs::write(month.join(format!("{stem} (1).json")), b"{}").unwrap();
        fs::write(month.join(format!("{stem}.json.part")), b"{").unwrap();
        fs::write(tmp.path().join(OPS_DIR).join("trasig.json"), b"{{{").unwrap();
        fs::create_dir_all(month.join("undermapp")).unwrap();

        let scan = scan_ops(tmp.path()).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].name, good);
        assert_eq!(scan.ignored.len(), 6);
    }

    #[test]
    fn untrusted_files_never_crash() {
        let tmp = TempDir::new();
        ensure_layout(tmp.path()).unwrap();
        let op = sample_op();

        // File too large.
        let big = vec![b' '; MAX_FILE_BYTES as usize + 1];
        let entry = plant(tmp.path(), &op, &big);
        assert!(matches!(read_op_file(&entry), Err(ReadError::Invalid(_))));

        // Broken or truncated JSON: try again later.
        for content in [
            &b"{\"v\": 1, \"op_id\": \"abc"[..],
            b"",
            b"   \n",
            b"hej",
            b"\xff\xfe\x00",
            &op.json.as_bytes()[..op.json.len() / 2],
        ] {
            let entry = plant(tmp.path(), &op, content);
            assert!(
                matches!(read_op_file(&entry), Err(ReadError::Incomplete(_))),
                "{content:?}"
            );
        }

        // Valid JSON but unknown schema.
        for content in [
            "[]",
            "null",
            "42",
            "{}",
            "{\"v\": 0}",
            "{\"v\": \"1\"}",
            "{\"v\": 1}",
            "{\"v\": 1, \"hej\": true}",
            "{\"v\": 2}",
        ] {
            let entry = plant(tmp.path(), &op, content.as_bytes());
            assert!(
                matches!(read_op_file(&entry), Err(ReadError::Invalid(_))),
                "{content}"
            );
        }

        // Extra fields in a v1 event are rejected (strict parsing).
        let mut value: Value = serde_json::from_str(&op.json).unwrap();
        value["extra"] = json!(true);
        let entry = plant(tmp.path(), &op, value.to_string().as_bytes());
        assert!(matches!(read_op_file(&entry), Err(ReadError::Invalid(_))));

        // The content belongs to a different event than the file name says.
        let other = sample_op();
        let entry = plant(tmp.path(), &op, other.json.as_bytes());
        assert!(matches!(read_op_file(&entry), Err(ReadError::Invalid(_))));

        // Right name but wrong month folder.
        let wrong_dir = tmp.path().join(OPS_DIR).join("1999-01");
        fs::create_dir_all(&wrong_dir).unwrap();
        let name = file_name(&op);
        fs::write(wrong_dir.join(&name), op.json.as_bytes()).unwrap();
        let entry = ScanEntry {
            rel: format!("1999-01/{name}"),
            dir: "1999-01".to_string(),
            path: wrong_dir.join(&name),
            name,
        };
        assert!(matches!(read_op_file(&entry), Err(ReadError::Invalid(_))));

        // The real file can still be read.
        let entry = plant(tmp.path(), &op, op.json.as_bytes());
        assert_eq!(read_op_file(&entry).unwrap(), op);
    }

    fn with_payload(op: &Op, kind: &str, payload: Value) -> Op {
        let mut value: Value = serde_json::from_str(&op.json).unwrap();
        value["kind"] = json!(kind);
        value["payload"] = payload;
        Op::parse(&value.to_string()).expect("the envelope is valid")
    }

    #[test]
    fn javascript_link_as_drive_location_is_rejected() {
        let op = sample_op();
        let bad = with_payload(
            &op,
            "finish",
            json!({
                "assignee": "Bo",
                "report_kind": "drive",
                "report_ref": "javascript:alert(1)",
                "results_kind": "",
                "results_ref": "",
                "summary": "Klart",
            }),
        );
        assert!(matches!(bad.action, Action::Invalid(_)));

        let bad_input = with_payload(
            &op,
            "create",
            json!({
                "title": "T", "description": "", "sim_type": "", "priority": 2,
                "requester": "A", "deadline": "",
                "input_kind": "drive", "input_ref": "file:///etc/passwd",
            }),
        );
        assert!(matches!(bad_input.action, Action::Invalid(_)));

        // The same text as free text under "Other" is allowed, but never becomes a link.
        let as_text = with_payload(
            &op,
            "finish",
            json!({
                "assignee": "Bo",
                "report_kind": "other",
                "report_ref": "javascript:alert(1)",
                "results_kind": "",
                "results_ref": "",
                "summary": "Klart",
            }),
        );
        match as_text.action {
            Action::Finish(p) => {
                assert_eq!(p.report.kind, LocKind::Other);
                assert!(!crate::model::is_http_url(&p.report.value));
            }
            other => panic!("unexpected: {other:?}"),
        }

        // The app's own interface cannot create such an event either.
        let own = build_op(
            Uuid::new_v4(),
            hlc(1, 0),
            "Bo",
            Uuid::new_v4(),
            &NewAction::Finish {
                assignee: "Bo".to_string(),
                report: Some(Location {
                    kind: LocKind::Drive,
                    value: "javascript:alert(1)".to_string(),
                }),
                results: None,
                summary: "Klart".to_string(),
            },
        )
        .unwrap();
        assert!(matches!(own.action, Action::Invalid(_)));
    }

    #[test]
    fn payload_limits_and_strictness() {
        let op = sample_op();
        let create = |title: String, priority: i64, deadline: &str| {
            with_payload(
                &op,
                "create",
                json!({
                    "title": title, "description": "", "sim_type": "", "priority": priority,
                    "requester": "A", "deadline": deadline, "input_kind": "", "input_ref": "",
                }),
            )
            .action
        };
        assert!(matches!(
            create("ö".repeat(MAX_TITLE), 1, ""),
            Action::Create(_)
        ));
        assert!(matches!(
            create("ö".repeat(MAX_TITLE + 1), 1, ""),
            Action::Invalid(_)
        ));
        assert!(matches!(
            create("  ".to_string(), 1, ""),
            Action::Invalid(_)
        ));
        assert!(matches!(create("T".to_string(), 0, ""), Action::Invalid(_)));
        assert!(matches!(create("T".to_string(), 4, ""), Action::Invalid(_)));
        assert!(matches!(
            create("T".to_string(), -1, ""),
            Action::Invalid(_)
        ));
        assert!(matches!(
            create("T".to_string(), 2, "snart"),
            Action::Invalid(_)
        ));

        // Unknown fields in the payload, wrong types and missing fields.
        assert!(matches!(
            with_payload(&op, "take", json!({"assignee": "Bo", "x": 1})).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "take", json!({"assignee": 5})).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "take", json!({})).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "take", json!("hej")).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "fail", json!({"note": "x".repeat(MAX_TEXT + 1)})).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "requeue", json!({"note": "x"})).action,
            Action::Invalid(_)
        ));
        assert!(matches!(
            with_payload(&op, "requeue", json!({})).action,
            Action::Requeue
        ));
    }

    #[test]
    fn unknown_kind_and_newer_version_are_kept_but_have_no_effect() {
        let op = sample_op();
        let unknown = with_payload(&op, "archive", json!({"whatever": [1, 2, 3]}));
        assert_eq!(unknown.action, Action::Unsupported);
        assert_eq!(unknown.op_id, op.op_id);

        let mut value: Value = serde_json::from_str(&op.json).unwrap();
        value["v"] = json!(SUPPORTED_V + 1);
        value["new_field"] = json!({"a": 1});
        value["payload"] = json!("helt annat format");
        let newer = Op::parse(&value.to_string()).unwrap();
        assert_eq!(newer.v, SUPPORTED_V + 1);
        assert_eq!(newer.action, Action::Unsupported);
        assert_eq!(newer.key(), op.key());
    }

    #[test]
    fn layout_is_created_and_newer_format_is_refused() {
        let tmp = TempDir::new();
        assert_eq!(
            ensure_layout(&tmp.path().join("finns-inte")),
            Err(FolderError::NotFound)
        );

        ensure_layout(tmp.path()).unwrap();
        assert!(tmp.path().join(OPS_DIR).is_dir());
        let text = fs::read_to_string(tmp.path().join(FORMAT_FILE)).unwrap();
        assert_eq!(text, "{\"format_version\":1}");
        // The second time nothing changes.
        ensure_layout(tmp.path()).unwrap();

        let newer = TempDir::new();
        fs::write(newer.path().join(FORMAT_FILE), "{\"format_version\": 2}").unwrap();
        assert_eq!(
            ensure_layout(newer.path()),
            Err(FolderError::NewerFormat(2))
        );
        assert!(!newer.path().join(OPS_DIR).exists());

        let broken = TempDir::new();
        fs::write(broken.path().join(FORMAT_FILE), "{\"format_ver").unwrap();
        assert!(matches!(
            ensure_layout(broken.path()),
            Err(FolderError::BadFormat(_))
        ));
    }

    #[test]
    fn uploaded_files_need_the_newer_event_version() {
        let sha = "ab".repeat(32);
        let file = Location {
            kind: LocKind::File,
            value: format!("sha256:{sha}:1234:Monokock.step"),
        };
        let with_file = NewAction::Create {
            title: "T".to_string(),
            description: String::new(),
            sim_type: String::new(),
            priority: 2,
            requester: "A".to_string(),
            deadline: String::new(),
            input: Some(file.clone()),
        };
        let op = build_op(Uuid::new_v4(), hlc(1, 0), "A", Uuid::new_v4(), &with_file).unwrap();
        assert_eq!(op.v, FILES_V);
        assert!(matches!(&op.action, Action::Create(p) if p.input == Some(file)));

        // Events without files keep version 1, so older apps still read them.
        let plain = build_op(
            Uuid::new_v4(),
            hlc(1, 0),
            "A",
            Uuid::new_v4(),
            &create_action("T"),
        )
        .unwrap();
        assert_eq!(plain.v, 1);

        // A version 1 event may not refer to an uploaded file.
        let mut value: Value = serde_json::from_str(&op.json).unwrap();
        value["v"] = json!(1);
        let downgraded = Op::parse(&value.to_string()).unwrap();
        assert!(matches!(downgraded.action, Action::Invalid(_)));

        // A broken file reference is rejected in any version.
        let mut value: Value = serde_json::from_str(&op.json).unwrap();
        value["payload"]["input_ref"] = json!("sha256:kort:1:x");
        assert!(matches!(
            Op::parse(&value.to_string()).unwrap().action,
            Action::Invalid(_)
        ));
        value["payload"]["input_ref"] = json!("../../etc/passwd");
        assert!(matches!(
            Op::parse(&value.to_string()).unwrap().action,
            Action::Invalid(_)
        ));
    }
}
