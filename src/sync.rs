//! Sync with the shared folder.
//!
//! `Client` contains all the logic (reading the folder, submitting own events) and
//! is free of threads so that it can be tested. `spawn` starts the background thread
//! that owns the client, so that the user interface never does file I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::config::{self, Config, Paths};
use crate::github::{self, GithubConfig, GithubRemote, RemoteStatus, Transport};
use crate::hlc::Clock;
use crate::i18n::{self, is_en, t, Lang};
use crate::model::{one_line, truncate_chars, Request, MAX_NAME};
use crate::ops::{self, FolderError, NewAction, Op, ReadError};
use crate::replay::{Outcome, State};
use crate::store::{Seen, SeenState, Store, StoreError};

/// How many times we try to read a file that cannot be parsed.
pub const MAX_ATTEMPTS: u32 = 20;
/// The folder is read at this interval.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(15);
const FOCUS_SYNC_MIN_GAP: Duration = Duration::from_secs(3);
const META_FOLDER: &str = "folder";

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderStatus {
    NotConfigured,
    /// The first sync has not run yet.
    Starting,
    Ok,
    /// The folder is missing (e.g. Drive unmounted): reading from the cache works.
    Missing,
    /// The folder requires a newer app: nothing may be written.
    NewerFormat,
    Error(String),
}

/// A message to show as a dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub new_ops: usize,
    pub full_replay: bool,
    /// Own events that applied before but are now rejected.
    pub conflicts: Vec<Notice>,
}

#[derive(Debug)]
pub enum Submitted {
    Applied {
        request_id: Uuid,
        op_id: Uuid,
    },
    /// Someone got there first: nothing was written.
    Rejected(Notice),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub at_ms: u64,
    pub by: String,
    pub what: String,
    /// `None` if the event was applied, otherwise why it does not apply.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagOp {
    pub at_ms: u64,
    pub by: String,
    pub what: String,
    pub title: String,
    pub problem: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagFile {
    pub path: String,
    pub state: SeenState,
    pub attempts: u32,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedRequest {
    pub seq: u32,
    pub title: String,
    pub by: String,
    pub at_ms: u64,
}

/// Everything the user interface needs, as an immutable snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub requests: Vec<Request>,
    /// Requests that have been deleted (shown under diagnostics only).
    pub deleted: Vec<DeletedRequest>,
    pub history: HashMap<Uuid, Vec<HistoryEntry>>,
    pub status: FolderStatus,
    pub last_sync_ms: Option<u64>,
    pub op_count: usize,
    pub unsupported_seen: bool,
    pub bad_files: Vec<DiagFile>,
    pub ignored_files: Vec<String>,
    pub rejected_ops: Vec<DiagOp>,
    pub folder: Option<PathBuf>,
    pub client_id: Uuid,
    /// The status towards GitHub, if the app syncs with a repository.
    pub remote: Option<RemoteStatus>,
}

pub struct Client {
    store: Store,
    clock: Clock,
    client_id: Uuid,
    display_name: String,
    folder: Option<PathBuf>,
    status: FolderStatus,
    last_sync_ms: Option<u64>,
    ignored: Vec<String>,
    remote: Option<RemoteStatus>,
}

impl Client {
    pub fn new(
        mut store: Store,
        client_id: Uuid,
        display_name: String,
        folder: Option<PathBuf>,
    ) -> Result<Client, StoreError> {
        if let Some(folder) = &folder {
            bind_cache(&mut store, folder)?;
        }
        let mut clock = Clock::new();
        let now = now_ms();
        for op in store.ops() {
            clock.observe(op.hlc, now);
        }
        let status = if folder.is_some() {
            FolderStatus::Starting
        } else {
            FolderStatus::NotConfigured
        };
        Ok(Client {
            store,
            clock,
            client_id,
            display_name,
            folder,
            status,
            last_sync_ms: None,
            ignored: Vec::new(),
            remote: None,
        })
    }

    pub fn client_id(&self) -> Uuid {
        self.client_id
    }

    pub fn state(&self) -> &State {
        self.store.state()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn set_remote(&mut self, remote: Option<RemoteStatus>) {
        self.remote = remote;
    }

    pub fn status(&self) -> &FolderStatus {
        &self.status
    }

    /// Changes display name and/or folder. If the folder changes the cache is emptied.
    pub fn set_identity(
        &mut self,
        display_name: String,
        folder: PathBuf,
    ) -> Result<(), StoreError> {
        bind_cache(&mut self.store, &folder)?;
        if self.folder.as_deref() != Some(folder.as_path()) {
            self.status = FolderStatus::Starting;
            self.last_sync_ms = None;
            self.ignored.clear();
        }
        self.display_name = display_name;
        self.folder = Some(folder);
        Ok(())
    }

    /// Reads the folder: lists `ops/**/*.json`, reads only files that are not already
    /// finished in `seen_files`, and replays the new events.
    pub fn sync(&mut self) -> SyncReport {
        let mut report = SyncReport::default();
        let Some(folder) = self.folder.clone() else {
            self.status = FolderStatus::NotConfigured;
            return report;
        };
        let mut status = match ops::ensure_layout(&folder) {
            Ok(()) => FolderStatus::Ok,
            Err(FolderError::NotFound) => {
                self.status = FolderStatus::Missing;
                return report;
            }
            Err(FolderError::NewerFormat(_)) => FolderStatus::NewerFormat,
            Err(e) => {
                self.status = FolderStatus::Error(e.to_string());
                return report;
            }
        };
        let scan = match ops::scan_ops(&folder) {
            Ok(scan) => scan,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Newer format without an ops folder, or the folder disappeared just now.
                self.status = if status == FolderStatus::NewerFormat {
                    status
                } else {
                    FolderStatus::Missing
                };
                return report;
            }
            Err(e) => {
                self.status = FolderStatus::Error(format!(
                    "{}: {e}",
                    t("Kan inte läsa mappen", "Cannot read the folder")
                ));
                return report;
            }
        };

        let mut new_ops = Vec::new();
        let mut seen_updates = Vec::new();
        for entry in &scan.entries {
            let attempts = match self.store.seen().get(&entry.rel) {
                Some(seen) if seen.state != SeenState::Retry => continue,
                Some(seen) => seen.attempts,
                None => 0,
            };
            match ops::read_op_file(entry) {
                Ok(op) => {
                    new_ops.push(op);
                    seen_updates.push((entry.rel.clone(), Seen::ok()));
                }
                Err(ReadError::Incomplete(error)) => {
                    let attempts = attempts + 1;
                    let state = if attempts >= MAX_ATTEMPTS {
                        SeenState::Corrupt
                    } else {
                        SeenState::Retry
                    };
                    seen_updates.push((
                        entry.rel.clone(),
                        Seen {
                            state,
                            attempts,
                            error: truncate_chars(&error, 300),
                        },
                    ));
                }
                Err(ReadError::Invalid(error)) => {
                    seen_updates.push((
                        entry.rel.clone(),
                        Seen {
                            state: SeenState::Invalid,
                            attempts: attempts + 1,
                            error: truncate_chars(&error, 300),
                        },
                    ));
                }
            }
        }

        let now = now_ms();
        for op in &new_ops {
            self.clock.observe(op.hlc, now);
        }
        let ingest = match self.store.ingest(new_ops) {
            Ok(ingest) => ingest,
            Err(e) => {
                self.status = FolderStatus::Error(e.to_string());
                return report;
            }
        };
        if let Err(e) = self.store.mark_seen(seen_updates) {
            self.status = FolderStatus::Error(e.to_string());
            return report;
        }

        report.new_ops = ingest.added;
        report.full_replay = ingest.full_replay;
        for op_id in &ingest.flipped {
            let own = self
                .store
                .ops()
                .iter()
                .find(|op| op.op_id == *op_id && op.client_id == self.client_id);
            if let Some(op) = own {
                if let Some(outcome) = self.store.state().outcomes.get(op_id) {
                    report.conflicts.push(self.conflict_notice(op, outcome));
                }
            }
        }

        if let Some(error) = scan.errors.first() {
            status = FolderStatus::Error(format!(
                "{}: {error}",
                t(
                    "Kunde inte läsa hela mappen",
                    "Could not read the whole folder"
                )
            ));
        }
        self.ignored = scan.ignored;
        self.last_sync_ms = Some(now);
        self.status = status;
        report
    }

    /// Submits an own event: writes the file and replays it locally at the same
    /// time, so that the user interface responds immediately.
    pub fn submit(
        &mut self,
        request_id: Option<Uuid>,
        action: &NewAction,
    ) -> Result<Submitted, String> {
        let folder = self.folder.clone().ok_or_else(|| {
            t("Ingen delad mapp är vald.", "No shared folder is selected.").to_string()
        })?;
        match ops::ensure_layout(&folder) {
            Ok(()) => {}
            Err(FolderError::NotFound) => {
                self.status = FolderStatus::Missing;
                return Err(
                    t("Mappen hittas inte, så inget kan sparas just nu. Kontrollera att Google Drive är igång.", "The folder cannot be found, so nothing can be saved right now. Check that Google Drive is running.")
                        .to_string(),
                );
            }
            Err(e @ FolderError::NewerFormat(_)) => {
                self.status = FolderStatus::NewerFormat;
                return Err(e.to_string());
            }
            Err(e) => return Err(e.to_string()),
        }

        let request_id = request_id.unwrap_or_else(Uuid::new_v4);
        let hlc = self.clock.tick(now_ms());
        let op = ops::build_op(self.client_id, hlc, &self.display_name, request_id, action)?;
        if let ops::Action::Invalid(reason) = &op.action {
            return Err(format!(
                "{}: {reason}.",
                t("Det går inte att spara", "Cannot save")
            ));
        }

        // Do not write anything we already know will not apply.
        match self.store.state().preview(&op) {
            Outcome::Applied => {}
            Outcome::Waiting => {
                return Err(t(
                    "Ärendet finns inte längre i listan.",
                    "The request is no longer in the list.",
                )
                .to_string())
            }
            outcome => return Ok(Submitted::Rejected(self.conflict_notice(&op, &outcome))),
        }

        let rel = ops::write_op(&folder, &op).map_err(|e| {
            format!(
                "{}: {e}",
                t(
                    "Kunde inte skriva till den delade mappen",
                    "Could not write to the shared folder"
                )
            )
        })?;
        let op_id = op.op_id;
        // The file is written. A cache error here resolves itself at the next sync.
        self.store.ingest(vec![op]).map_err(|e| e.to_string())?;
        self.store
            .mark_seen(vec![(rel, Seen::ok())])
            .map_err(|e| e.to_string())?;
        Ok(Submitted::Applied { request_id, op_id })
    }

    /// "Anna took the request before you, so your change does not apply. The request is in progress (Anna)."
    fn conflict_notice(&self, op: &Op, outcome: &Outcome) -> Notice {
        let request = self.store.state().requests.get(&op.request_id);
        let mut text = match outcome {
            Outcome::Rejected {
                blocker: Some(blocker),
                ..
            } => {
                let who = if blocker.by.trim().is_empty() {
                    t("Någon", "Someone").to_string()
                } else {
                    one_line(&blocker.by)
                };
                let verb = blocker.kind.verb();
                if is_en() {
                    format!("{who} {verb} before you, so your change does not apply.")
                } else {
                    format!("{who} {verb} före dig, så din ändring gäller inte.")
                }
            }
            Outcome::Rejected { reason, .. } => {
                format!(
                    "{} {}.",
                    t("Din ändring gäller inte.", "Your change does not apply."),
                    reason.text()
                )
            }
            _ => t("Din ändring gäller inte.", "Your change does not apply.").to_string(),
        };
        let headline = t("Någon hann före", "Someone got there first");
        let mut title = headline.to_string();
        if let Some(request) = request {
            if request.deleted_at.is_some() {
                text.push_str(t(" Ärendet är borttaget", " The request has been deleted"));
            } else {
                if is_en() {
                    text.push_str(&format!(
                        " The request is {}",
                        request.status.label().to_lowercase()
                    ));
                } else {
                    text.push_str(&format!(" Ärendet är {}", request.status.label()));
                }
                if !request.assignee.trim().is_empty() {
                    text.push_str(&format!(" ({})", one_line(&request.assignee)));
                }
            }
            text.push('.');
            title = format!(
                "{headline}: {}",
                truncate_chars(&one_line(&request.title), 60)
            );
        }
        Notice { title, text }
    }

    /// Empties the cache and reads everything from the folder again. Only done when
    /// the folder is reachable, otherwise the saved data would disappear.
    pub fn rebuild(&mut self) -> Result<SyncReport, String> {
        let folder = self.folder.clone().ok_or_else(|| {
            t("Ingen delad mapp är vald.", "No shared folder is selected.").to_string()
        })?;
        match ops::ensure_layout(&folder) {
            Ok(()) | Err(FolderError::NewerFormat(_)) => {}
            Err(e) => {
                return Err(format!(
                    "{} {e}",
                    t(
                        "Cachen kan bara byggas om när mappen går att nå.",
                        "The cache can only be rebuilt when the folder is reachable."
                    )
                ))
            }
        }
        self.store.clear().map_err(|e| e.to_string())?;
        Ok(self.sync())
    }

    /// Re-parses the stored events so that error messages created
    /// while reading get the new language.
    pub fn retranslate(&mut self) {
        let _ = self.store.reload();
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.store.state();
        // Deleted requests are hidden from the list but listed under diagnostics.
        let mut requests: Vec<Request> = state
            .requests
            .values()
            .filter(|r| r.deleted_at.is_none())
            .cloned()
            .collect();
        requests.sort_by_key(|r| r.seq);
        let mut deleted: Vec<DeletedRequest> = state
            .requests
            .values()
            .filter_map(|r| {
                r.deleted_at.map(|at_ms| DeletedRequest {
                    seq: r.seq,
                    title: r.title.clone(),
                    by: r.last_by.clone(),
                    at_ms,
                })
            })
            .collect();
        deleted.sort_by_key(|d| d.at_ms);

        let mut history: HashMap<Uuid, Vec<HistoryEntry>> = HashMap::new();
        let mut rejected_ops = Vec::new();
        for op in self.store.ops() {
            let what = match op.op_kind() {
                Some(kind) => kind.label().to_string(),
                None => format!(
                    "{} ({})",
                    t("Okänd händelse", "Unknown event"),
                    truncate_chars(&one_line(&op.kind), 30)
                ),
            };
            let problem = match state.outcomes.get(&op.op_id) {
                Some(Outcome::Applied) => None,
                Some(Outcome::Rejected { reason, .. }) => {
                    Some(format!("{}: {}", t("Avvisad", "Rejected"), reason.text()))
                }
                Some(Outcome::Waiting) => Some(
                    t(
                        "Väntar på att ärendet ska synkas",
                        "Waiting for the request to sync",
                    )
                    .to_string(),
                ),
                Some(Outcome::Ignored) | None => Some(
                    t(
                        "Ignorerad: kräver en nyare version av appen",
                        "Ignored: requires a newer version of the app",
                    )
                    .to_string(),
                ),
            };
            if let Some(problem) = &problem {
                rejected_ops.push(DiagOp {
                    at_ms: op.hlc.wall_ms,
                    by: op.by.clone(),
                    what: what.clone(),
                    title: state
                        .requests
                        .get(&op.request_id)
                        .map(|r| r.title.clone())
                        .unwrap_or_default(),
                    problem: problem.clone(),
                });
            }
            history
                .entry(op.request_id)
                .or_default()
                .push(HistoryEntry {
                    at_ms: op.hlc.wall_ms,
                    by: op.by.clone(),
                    what,
                    problem,
                });
        }

        let mut bad_files: Vec<DiagFile> = self
            .store
            .seen()
            .iter()
            .filter(|(_, seen)| seen.state != SeenState::Ok)
            .map(|(path, seen)| DiagFile {
                path: path.clone(),
                state: seen.state,
                attempts: seen.attempts,
                error: seen.error.clone(),
            })
            .collect();
        bad_files.sort_by(|a, b| a.path.cmp(&b.path));

        Snapshot {
            requests,
            deleted,
            history,
            status: self.status.clone(),
            last_sync_ms: self.last_sync_ms,
            op_count: self.store.ops().len(),
            unsupported_seen: state.unsupported_seen,
            bad_files,
            ignored_files: self.ignored.clone(),
            rejected_ops,
            folder: self.folder.clone(),
            client_id: self.client_id,
            remote: self.remote.clone(),
        }
    }
}

/// A cache belongs to one particular folder. If another folder is chosen the cache is emptied.
fn bind_cache(store: &mut Store, folder: &Path) -> Result<(), StoreError> {
    let wanted = folder.to_string_lossy().into_owned();
    if store.meta_get(META_FOLDER)?.as_deref() != Some(wanted.as_str()) {
        store.clear()?;
        store.meta_set(META_FOLDER, &wanted)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The background thread
// ---------------------------------------------------------------------------

pub enum Cmd {
    /// The "Sync now" button.
    SyncNow,
    /// The window got focus (skipped if we just synced).
    FocusSync,
    Submit {
        token: u64,
        request_id: Option<Uuid>,
        action: NewAction,
    },
    Configure {
        token: u64,
        display_name: String,
        source: Source,
    },
    /// Report from the thread that mirrors against GitHub.
    Remote {
        status: RemoteStatus,
        new_files: bool,
    },
    Rebuild,
    /// Changes language: saved in the settings and all texts are rebuilt.
    SetLanguage(Lang),
    Shutdown,
}

/// Where the events should be synced.
pub enum Source {
    /// A shared folder, e.g. in Google Drive for desktop.
    Folder(PathBuf),
    /// A private GitHub repository: `owner/repo:token`.
    TeamKey(String),
    /// Keep what is already configured (only the name changes).
    Keep,
}

pub enum Event {
    SubmitDone {
        token: u64,
        result: Result<(), String>,
    },
    ConfigureDone {
        token: u64,
        result: Result<Config, String>,
    },
    Notice(Notice),
}

pub struct Handle {
    tx: Sender<Cmd>,
    pub events: Receiver<Event>,
    shared: Arc<Mutex<Arc<Snapshot>>>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.shared.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

/// Starts the background thread. `notify` is called every time there is something
/// new to show (the user interface uses it to repaint).
pub fn spawn(
    client: Client,
    config: Config,
    paths: Paths,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<Handle> {
    let (tx, rx) = mpsc::channel();
    let (event_tx, events) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Arc::new(client.snapshot())));
    let mut worker = Worker {
        client,
        config,
        paths,
        tx: tx.clone(),
        transport: None,
        rx,
        events: event_tx,
        shared: Arc::clone(&shared),
        notify,
        last_sync: None,
    };
    worker.start_transport();
    *shared.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(worker.client.snapshot());
    std::thread::Builder::new()
        .name("simq-sync".to_string())
        .spawn(move || worker.run())?;
    Ok(Handle { tx, events, shared })
}

struct Worker {
    client: Client,
    config: Config,
    paths: Paths,
    tx: Sender<Cmd>,
    transport: Option<Transport>,
    rx: Receiver<Cmd>,
    events: Sender<Event>,
    shared: Arc<Mutex<Arc<Snapshot>>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    last_sync: Option<Instant>,
}

impl Worker {
    fn run(mut self) {
        loop {
            let wait = match self.last_sync {
                Some(at) => SYNC_INTERVAL.saturating_sub(at.elapsed()),
                None => Duration::ZERO,
            };
            match self.rx.recv_timeout(wait) {
                Err(RecvTimeoutError::Timeout) => self.sync(),
                Ok(Cmd::SyncNow) => {
                    self.wake_transport();
                    self.sync();
                }
                Ok(Cmd::Remote { status, new_files }) => {
                    // Reports from a transport that has been replaced are ignored.
                    let current = self.config.github.as_ref().map(|g| g.repo.as_str());
                    if current == Some(status.repo.as_str()) {
                        self.client.set_remote(Some(status));
                        if new_files {
                            self.sync();
                        }
                    }
                }
                Ok(Cmd::FocusSync) => {
                    if self
                        .last_sync
                        .is_none_or(|at| at.elapsed() >= FOCUS_SYNC_MIN_GAP)
                    {
                        self.sync();
                    }
                }
                Ok(Cmd::Submit {
                    token,
                    request_id,
                    action,
                }) => {
                    let result = match self.client.submit(request_id, &action) {
                        Ok(Submitted::Applied { .. }) => {
                            self.wake_transport();
                            Ok(())
                        }
                        Ok(Submitted::Rejected(notice)) => {
                            self.emit(Event::Notice(notice));
                            Ok(())
                        }
                        Err(e) => Err(e),
                    };
                    self.emit(Event::SubmitDone { token, result });
                }
                Ok(Cmd::Configure {
                    token,
                    display_name,
                    source,
                }) => {
                    let result = self.configure(display_name, source);
                    let ok = result.is_ok();
                    self.emit(Event::ConfigureDone { token, result });
                    if ok {
                        self.sync();
                    }
                }
                Ok(Cmd::SetLanguage(lang)) => {
                    i18n::set(lang);
                    self.client.retranslate();
                    self.config.language = lang;
                    if let Err(e) = config::save(&self.paths, &self.config) {
                        self.emit(Event::Notice(Notice {
                            title: t("Språket sparades inte", "The language was not saved")
                                .to_string(),
                            text: e.to_string(),
                        }));
                    }
                }
                Ok(Cmd::Rebuild) => match self.client.rebuild() {
                    Ok(_) => self.last_sync = Some(Instant::now()),
                    Err(text) => self.emit(Event::Notice(Notice {
                        title: t("Cachen byggdes inte om", "The cache was not rebuilt").to_string(),
                        text,
                    })),
                },
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            }
            self.publish();
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn sync(&mut self) {
        let report = self.client.sync();
        self.last_sync = Some(Instant::now());
        for notice in report.conflicts {
            self.emit(Event::Notice(notice));
        }
    }

    fn wake_transport(&self) {
        if let Some(transport) = &self.transport {
            transport.wake();
        }
    }

    /// (Re)starts the thread that mirrors against GitHub, if a repository is configured.
    fn start_transport(&mut self) {
        self.transport = None;
        let Some(github) = self.config.github.clone() else {
            self.client.set_remote(None);
            return;
        };
        let folder = self.paths.mirror_folder(&github.repo);
        let _ = std::fs::create_dir_all(&folder);
        self.client
            .set_remote(Some(RemoteStatus::starting(&github.repo)));
        let tx = self.tx.clone();
        let started = github::spawn_transport(
            Box::new(GithubRemote::new(&github)),
            github.repo.clone(),
            folder,
            Box::new(move |status, new_files| {
                let _ = tx.send(Cmd::Remote { status, new_files });
            }),
        );
        match started {
            Ok(transport) => self.transport = Some(transport),
            Err(e) => self.emit(Event::Notice(Notice {
                title: t(
                    "GitHub-synken startade inte",
                    "The GitHub sync did not start",
                )
                .to_string(),
                text: e.to_string(),
            })),
        }
    }

    fn configure(&mut self, display_name: String, source: Source) -> Result<Config, String> {
        let display_name = display_name.trim().to_string();
        if display_name.is_empty() {
            return Err(t("Ange ett visningsnamn.", "Enter a display name.").to_string());
        }
        if crate::model::char_len(&display_name) > MAX_NAME {
            return Err(t(
                "Visningsnamnet är för långt.",
                "The display name is too long.",
            )
            .to_string());
        }
        let mut config = self.config.clone();
        config.display_name = display_name.clone();
        let folder = match source {
            Source::Folder(folder) => {
                match ops::ensure_layout(&folder) {
                    // A folder with a newer format may be chosen, but then it can only be read.
                    Ok(()) | Err(FolderError::NewerFormat(_)) => {}
                    Err(FolderError::NotFound) => return Err(t(
                        "Mappen finns inte. Välj en mapp som finns på datorn.",
                        "The folder does not exist. Choose a folder that exists on this computer.",
                    )
                    .to_string()),
                    Err(e) => return Err(e.to_string()),
                }
                config.sync_folder = Some(folder.clone());
                config.github = None;
                folder
            }
            Source::TeamKey(key) => {
                let (repo, token) = github::parse_team_key(&key)?;
                // Check with GitHub that the key can reach the repository and may write.
                let branch = GithubRemote::check(&repo, &token).map_err(|e| e.text())?;
                let folder = self.paths.mirror_folder(&repo);
                std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
                config.github = Some(GithubConfig {
                    repo,
                    token,
                    branch,
                });
                config.sync_folder = None;
                folder
            }
            Source::Keep => config.effective_folder(&self.paths).ok_or_else(|| {
                t(
                    "Välj en mapp eller ange en teamnyckel.",
                    "Choose a folder or enter a team key.",
                )
                .to_string()
            })?,
        };
        config::save(&self.paths, &config).map_err(|e| e.to_string())?;
        self.client
            .set_identity(display_name, folder)
            .map_err(|e| e.to_string())?;
        self.config = config.clone();
        self.start_transport();
        Ok(config)
    }

    fn publish(&self) {
        let snapshot = Arc::new(self.client.snapshot());
        *self.shared.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
        (self.notify)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LocKind, Location, Status};
    use crate::ops::OPS_DIR;
    use crate::testutil::{create_action, finish_action, op_at, take_action, TempDir};
    use std::fs;

    fn client(folder: &Path, name: &str) -> Client {
        Client::new(
            Store::open_in_memory().unwrap(),
            Uuid::new_v4(),
            name.to_string(),
            Some(folder.to_path_buf()),
        )
        .unwrap()
    }

    fn applied(result: Result<Submitted, String>) -> Uuid {
        match result {
            Ok(Submitted::Applied { request_id, .. }) => request_id,
            other => panic!("expected an applied event, got {other:?}"),
        }
    }

    fn op_files(folder: &Path) -> Vec<PathBuf> {
        ops::scan_ops(folder)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.path)
            .collect()
    }

    /// Simulates Drive delivering files: copies everything that is missing.
    fn deliver(from: &Path, to: &Path) {
        for entry in ops::scan_ops(from).unwrap().entries {
            let dir = to.join(OPS_DIR).join(&entry.dir);
            fs::create_dir_all(&dir).unwrap();
            let target = dir.join(&entry.name);
            if !target.exists() {
                fs::copy(&entry.path, &target).unwrap();
            }
        }
    }

    #[test]
    fn two_clients_share_a_folder() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let mut b = client(shared.path(), "Bo");
        assert_eq!(a.sync().new_ops, 0);
        assert_eq!(*a.status(), FolderStatus::Ok);
        assert!(shared.path().join("format.json").is_file());

        // Create in A, sync B.
        let id = applied(a.submit(None, &create_action("Styvhet upright")));
        assert_eq!(a.state().requests[&id].status, Status::Queued);
        assert_eq!(b.sync().new_ops, 1);
        let seen_by_b = &b.state().requests[&id];
        assert_eq!(seen_by_b.title, "Styvhet upright");
        assert_eq!(seen_by_b.requester, "Anna");
        assert_eq!(seen_by_b.last_by, "Anna");

        // B takes the request, sync A.
        applied(b.submit(Some(id), &take_action("Bo")));
        assert_eq!(a.sync().new_ops, 1);
        assert_eq!(a.state().requests[&id].status, Status::Running);
        assert_eq!(a.state().requests[&id].assignee, "Bo");

        // B finishes with a Drive link as report and a Vault description as results.
        applied(b.submit(Some(id), &finish_action("Bo")));
        let report = a.sync();
        assert_eq!(report.new_ops, 1);
        assert!(report.conflicts.is_empty());
        let done = &a.state().requests[&id];
        assert_eq!(done.status, Status::Done);
        assert_eq!(done.report.as_ref().unwrap().kind, LocKind::Drive);
        assert_eq!(done.results.as_ref().unwrap().kind, LocKind::Vault);
        assert_eq!(a.state(), b.state());

        // The files are in ops/YYYY-MM/ and no .part files are left.
        assert_eq!(op_files(shared.path()).len(), 3);
        assert!(ops::scan_ops(shared.path()).unwrap().ignored.is_empty());

        // A sync without news does not read any files again.
        assert_eq!(a.sync().new_ops, 0);
        assert_eq!(a.store().seen().len(), 3);
    }

    #[test]
    fn stale_click_is_rejected_without_writing_a_file() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let mut b = client(shared.path(), "Bo");
        let id = applied(a.submit(None, &create_action("Upright")));
        b.sync();
        applied(b.submit(Some(id), &take_action("Bo")));
        a.sync();

        // Anna clicks "Take request" in a view that has become stale.
        match a.submit(Some(id), &take_action("Anna")) {
            Ok(Submitted::Rejected(notice)) => {
                assert_eq!(
                    notice.text,
                    "Bo tog ärendet före dig, så din ändring gäller inte. Ärendet är Pågår (Bo)."
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(op_files(shared.path()).len(), 2);

        // Invalid input is stopped before anything is written.
        let bad = NewAction::Finish {
            assignee: "Anna".to_string(),
            report: Some(Location {
                kind: LocKind::Drive,
                value: "javascript:alert(1)".to_string(),
            }),
            results: None,
            summary: "Klart".to_string(),
        };
        assert!(a.submit(Some(id), &bad).is_err());
        assert_eq!(op_files(shared.path()).len(), 2);
    }

    #[test]
    fn drive_delay_both_take_and_both_end_up_with_the_same_winner() {
        // Two computers, each with its own local copy of the folder. "Drive" is `deliver`.
        let (dir_a, dir_b) = (TempDir::new(), TempDir::new());
        let mut a = client(dir_a.path(), "Anna");
        let mut b = client(dir_b.path(), "Bo");

        let id = applied(a.submit(None, &create_action("Aero-paket CFD")));
        deliver(dir_a.path(), dir_b.path());
        b.sync();
        assert_eq!(b.state().requests[&id].status, Status::Queued);

        // Both take the request before Drive has synced.
        applied(a.submit(Some(id), &take_action("Anna")));
        applied(b.submit(Some(id), &take_action("Bo")));
        assert_eq!(a.state().requests[&id].assignee, "Anna");
        assert_eq!(b.state().requests[&id].assignee, "Bo");

        // Drive delivers in both directions.
        deliver(dir_a.path(), dir_b.path());
        deliver(dir_b.path(), dir_a.path());
        let report_a = a.sync();
        let report_b = b.sync();

        // Both show the same winner.
        assert_eq!(a.state(), b.state());
        let winner = a.state().requests[&id].assignee.clone();
        assert_eq!(a.state().requests[&id].status, Status::Running);

        // Exactly one loser, and that is who gets the dialog.
        let (loser_report, winner_report, loser, other) = if winner == "Anna" {
            (&report_b, &report_a, "Bo", "Anna")
        } else {
            (&report_a, &report_b, "Anna", "Bo")
        };
        assert!(winner_report.conflicts.is_empty());
        assert_eq!(loser_report.conflicts.len(), 1, "{loser} förlorade");
        assert!(loser_report.full_replay);
        assert_eq!(
            loser_report.conflicts[0].text,
            format!(
                "{other} tog ärendet före dig, så din ändring gäller inte. Ärendet är Pågår ({other})."
            )
        );

        // The history shows both attempts, one of them rejected.
        let snapshot = a.snapshot();
        let history = &snapshot.history[&id];
        assert_eq!(history.len(), 3);
        assert_eq!(history.iter().filter(|h| h.problem.is_some()).count(), 1);
        assert_eq!(snapshot.rejected_ops.len(), 1);
        assert_eq!(snapshot.rejected_ops[0].by, loser);

        // The dialog is only shown once.
        assert!(a.sync().conflicts.is_empty());
        assert!(b.sync().conflicts.is_empty());
    }

    #[test]
    fn english_texts_when_english_is_selected() {
        use crate::i18n::{with_lang, Lang};
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let mut b = client(shared.path(), "Bo");
        let id = applied(a.submit(None, &create_action("Upright")));
        b.sync();
        applied(b.submit(Some(id), &take_action("Bo")));
        a.sync();

        with_lang(Lang::En, || {
            match a.submit(Some(id), &take_action("Anna")) {
                Ok(Submitted::Rejected(notice)) => {
                    assert_eq!(notice.title, "Someone got there first: Upright");
                    assert_eq!(
                        notice.text,
                        "Bo took the request before you, so your change does not apply. The request is in progress (Bo)."
                    );
                }
                other => panic!("unexpected: {other:?}"),
            }
            let snapshot = a.snapshot();
            let history = &snapshot.history[&id];
            assert_eq!(history[0].what, "Created the request");
            assert_eq!(history[1].what, "Took the request");
            assert_eq!(Status::Running.label(), "In progress");
            assert_eq!(crate::model::priority_label(1), "High");
            assert_eq!(LocKind::Other.label(), "Other");
            let missing = Client::new(
                Store::open_in_memory().unwrap(),
                Uuid::new_v4(),
                "Cia".to_string(),
                None,
            )
            .unwrap()
            .submit(None, &create_action("X"))
            .unwrap_err();
            assert_eq!(missing, "No shared folder is selected.");
            assert_eq!(
                FolderError::NewerFormat(3).to_string(),
                "The folder uses a newer format (version 3). Update the app."
            );
        });

        // The same client in Swedish again: no language has been stored in the data.
        assert_eq!(a.snapshot().history[&id][1].what, "Tog ärendet");
        assert_eq!(
            FolderError::NewerFormat(3).to_string(),
            "Mappen använder ett nyare format (version 3). Uppdatera appen."
        );
    }

    #[test]
    fn late_remote_take_with_earlier_clock_overrides_local_take() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let id = applied(a.submit(None, &create_action("Upright")));
        let created_at = a.state().requests[&id].created_at;
        // Anna's take must get a later millisecond than the create.
        std::thread::sleep(Duration::from_millis(5));
        applied(a.submit(Some(id), &take_action("Anna")));

        // A take from Cia with an earlier stamp shows up in the folder afterwards.
        let late = op_at(
            Uuid::new_v4(),
            created_at,
            5000,
            "Cia",
            id,
            &take_action("Cia"),
        );
        ops::write_op(shared.path(), &late).unwrap();
        let report = a.sync();
        assert!(report.full_replay);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            report.conflicts[0].text,
            "Cia tog ärendet före dig, så din ändring gäller inte. Ärendet är Pågår (Cia)."
        );
        assert_eq!(a.state().requests[&id].assignee, "Cia");
    }

    #[test]
    fn half_written_files_are_retried() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let mut b = client(shared.path(), "Bo");
        let id = applied(a.submit(None, &create_action("Upright")));
        b.sync();

        // B's take is first only visible as .part and then as truncated JSON.
        let take = op_at(
            b.client_id(),
            now_ms() + 10,
            0,
            "Bo",
            id,
            &take_action("Bo"),
        );
        let month = shared
            .path()
            .join(OPS_DIR)
            .join(ops::month_dir(take.hlc.wall_ms));
        fs::create_dir_all(&month).unwrap();
        let name = ops::file_name(&take);
        let rel = format!("{}/{name}", ops::month_dir(take.hlc.wall_ms));
        let part = month.join(format!("{name}.part"));
        fs::write(&part, &take.json.as_bytes()[..40]).unwrap();

        assert_eq!(a.sync().new_ops, 0);
        assert!(!a.store().seen().contains_key(&rel), ".part ignoreras helt");
        assert_eq!(a.snapshot().ignored_files.len(), 1);

        fs::rename(&part, month.join(&name)).unwrap();
        for attempt in 1..=3 {
            assert_eq!(a.sync().new_ops, 0);
            let seen = &a.store().seen()[&rel];
            assert_eq!(seen.state, SeenState::Retry);
            assert_eq!(seen.attempts, attempt);
        }
        assert_eq!(a.state().requests[&id].status, Status::Queued);
        assert_eq!(*a.status(), FolderStatus::Ok);

        // Drive finishes the file.
        fs::write(month.join(&name), take.json.as_bytes()).unwrap();
        assert_eq!(a.sync().new_ops, 1);
        assert_eq!(a.store().seen()[&rel].state, SeenState::Ok);
        assert_eq!(a.state().requests[&id].status, Status::Running);
        assert!(a.snapshot().bad_files.is_empty());
    }

    #[test]
    fn broken_file_becomes_corrupt_after_max_attempts() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        applied(a.submit(None, &create_action("Upright")));

        let ghost = op_at(
            Uuid::new_v4(),
            now_ms(),
            0,
            "Bo",
            Uuid::new_v4(),
            &take_action("Bo"),
        );
        let month = shared
            .path()
            .join(OPS_DIR)
            .join(ops::month_dir(ghost.hlc.wall_ms));
        fs::create_dir_all(&month).unwrap();
        let name = ops::file_name(&ghost);
        let rel = format!("{}/{name}", ops::month_dir(ghost.hlc.wall_ms));
        fs::write(month.join(&name), b"{ \"v\": 1, \"op_id\": ").unwrap();
        // A file with the wrong name directly in ops/ is simply ignored.
        fs::write(shared.path().join(OPS_DIR).join("trasig.json"), b"{{{").unwrap();

        for _ in 0..MAX_ATTEMPTS - 1 {
            a.sync();
            assert_eq!(a.store().seen()[&rel].state, SeenState::Retry);
        }
        a.sync();
        assert_eq!(a.store().seen()[&rel].state, SeenState::Corrupt);
        assert_eq!(a.store().seen()[&rel].attempts, MAX_ATTEMPTS);
        // After that the file is not read any more.
        a.sync();
        assert_eq!(a.store().seen()[&rel].attempts, MAX_ATTEMPTS);

        let snapshot = a.snapshot();
        assert_eq!(snapshot.bad_files.len(), 1);
        assert_eq!(snapshot.bad_files[0].state, SeenState::Corrupt);
        assert_eq!(snapshot.ignored_files, vec!["trasig.json".to_string()]);
        assert_eq!(snapshot.requests.len(), 1);
        assert_eq!(*a.status(), FolderStatus::Ok);
    }

    #[test]
    fn deleted_cache_is_rebuilt_to_identical_result() {
        let shared = TempDir::new();
        let local = TempDir::new();
        let cache = local.path().join("cache.sqlite");
        let client_id = Uuid::new_v4();
        let open = |cache: &Path| {
            Client::new(
                Store::open_or_recreate(cache).unwrap(),
                client_id,
                "Anna".to_string(),
                Some(shared.path().to_path_buf()),
            )
            .unwrap()
        };

        let mut a = open(&cache);
        let mut b = client(shared.path(), "Bo");
        let first = applied(a.submit(None, &create_action("Upright")));
        let second = applied(a.submit(None, &create_action("Aero")));
        b.sync();
        applied(b.submit(Some(first), &take_action("Bo")));
        applied(b.submit(Some(first), &finish_action("Bo")));
        applied(b.submit(
            Some(second),
            &NewAction::Cancel {
                note: "Behövs inte längre".to_string(),
            },
        ));
        // A rejected event must also be rebuilt the same way.
        let late = op_at(
            Uuid::new_v4(),
            now_ms() + 50,
            0,
            "Cia",
            first,
            &take_action("Cia"),
        );
        ops::write_op(shared.path(), &late).unwrap();
        a.sync();
        let state_before = a.state().clone();
        let dump_before = a.store().derived_dump().unwrap();
        assert_eq!(a.store().ops().len(), 6);
        drop(a);

        // Delete the cache file completely.
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{suffix}", cache.display()));
        }
        let mut a = open(&cache);
        assert!(a.state().requests.is_empty());
        assert_eq!(a.sync().new_ops, 6);
        assert_eq!(*a.state(), state_before);
        assert_eq!(a.store().derived_dump().unwrap(), dump_before);

        // The same with the "Rebuild cache" button.
        a.rebuild().unwrap();
        assert_eq!(*a.state(), state_before);
        assert_eq!(a.store().derived_dump().unwrap(), dump_before);

        // And after a restart without a sync everything is read from the cache.
        drop(a);
        let a = open(&cache);
        assert_eq!(*a.state(), state_before);
    }

    #[test]
    fn missing_folder_blocks_writes_but_cache_is_readable() {
        let parent = TempDir::new();
        let folder = parent.path().join("SimQ åäö");
        fs::create_dir_all(&folder).unwrap();
        let mut a = client(&folder, "Anna");
        let id = applied(a.submit(None, &create_action("Upright")));
        a.sync();

        // Drive is unmounted.
        let away = parent.path().join("borta");
        fs::rename(&folder, &away).unwrap();
        a.sync();
        assert_eq!(*a.status(), FolderStatus::Missing);
        assert_eq!(a.state().requests[&id].title, "Upright");
        assert_eq!(a.snapshot().requests.len(), 1);
        assert!(a.submit(Some(id), &take_action("Anna")).is_err());
        assert!(a.rebuild().is_err());
        assert_eq!(a.state().requests[&id].status, Status::Queued);
        assert!(
            !folder.exists(),
            "nothing may be created when the folder is missing"
        );

        // Drive comes back.
        fs::rename(&away, &folder).unwrap();
        a.sync();
        assert_eq!(*a.status(), FolderStatus::Ok);
        applied(a.submit(Some(id), &take_action("Anna")));
    }

    #[test]
    fn newer_folder_format_is_read_only() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let id = applied(a.submit(None, &create_action("Upright")));

        let newer = TempDir::new();
        fs::write(newer.path().join("format.json"), "{\"format_version\": 7}").unwrap();
        deliver(shared.path(), newer.path());
        let mut b = client(newer.path(), "Bo");
        assert_eq!(b.sync().new_ops, 1);
        assert_eq!(*b.status(), FolderStatus::NewerFormat);
        let error = b.submit(Some(id), &take_action("Bo")).unwrap_err();
        assert!(error.contains("Uppdatera appen"), "{error}");
        assert_eq!(op_files(newer.path()).len(), 1);
        assert_eq!(
            fs::read_to_string(newer.path().join("format.json")).unwrap(),
            "{\"format_version\": 7}"
        );
    }

    #[test]
    fn local_clock_follows_remote_ops_but_not_far_future_ones() {
        let shared = TempDir::new();
        let mut a = client(shared.path(), "Anna");
        let id = applied(a.submit(None, &create_action("Upright")));

        // Bo's clock is one hour ahead.
        let ahead = now_ms() + 3_600_000;
        let take = op_at(Uuid::new_v4(), ahead, 0, "Bo", id, &take_action("Bo"));
        ops::write_op(shared.path(), &take).unwrap();
        a.sync();
        assert_eq!(a.state().requests[&id].status, Status::Running);

        // Anna's next event still ends up AFTER Bo's and therefore applies.
        applied(a.submit(Some(id), &finish_action("Anna")));
        assert_eq!(a.state().requests[&id].status, Status::Done);
        assert!(a.state().requests[&id].finished_at.unwrap() >= ahead);

        // An event one week into the future is replayed but does not move the clock.
        let other = applied(a.submit(None, &create_action("Aero")));
        let far = now_ms() + 7 * 24 * 3_600_000;
        let far_take = op_at(Uuid::new_v4(), far, 0, "Dan", other, &take_action("Dan"));
        ops::write_op(shared.path(), &far_take).unwrap();
        a.sync();
        assert_eq!(a.state().requests[&other].assignee, "Dan");
        let third = applied(a.submit(None, &create_action("Kylning")));
        assert!(a.state().requests[&third].created_at < far);
    }

    #[test]
    fn worker_thread_syncs_and_reports() {
        let shared = TempDir::new();
        let local = TempDir::new();
        let paths = config::resolve_paths(Some(local.path())).unwrap();
        let cfg = config::load_or_init(&paths).unwrap();
        let store = Store::open_or_recreate(&paths.cache_file).unwrap();
        let client = Client::new(store, cfg.client_id, String::new(), None).unwrap();
        let handle = spawn(client, cfg, paths.clone(), Arc::new(|| {})).unwrap();

        let wait = |token: u64| loop {
            match handle.events.recv_timeout(Duration::from_secs(10)) {
                Ok(Event::SubmitDone { token: t, result }) if t == token => break result,
                Ok(Event::ConfigureDone { token: t, result }) if t == token => {
                    break result.map(|_| ())
                }
                Ok(_) => {}
                Err(e) => panic!("no reply from the background thread: {e}"),
            }
        };

        // Without a folder nothing can be saved.
        handle.send(Cmd::Submit {
            token: 1,
            request_id: None,
            action: create_action("Upright"),
        });
        assert!(wait(1).is_err());

        handle.send(Cmd::Configure {
            token: 2,
            display_name: "  Åsa  ".to_string(),
            source: Source::Folder(shared.path().to_path_buf()),
        });
        wait(2).unwrap();
        let saved = config::load_or_init(&paths).unwrap();
        assert_eq!(saved.display_name, "Åsa");
        assert_eq!(saved.sync_folder.as_deref(), Some(shared.path()));

        handle.send(Cmd::Submit {
            token: 3,
            request_id: None,
            action: create_action("Upright"),
        });
        wait(3).unwrap();
        handle.send(Cmd::SyncNow);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = handle.snapshot();
            if snapshot.requests.len() == 1 && snapshot.status == FolderStatus::Ok {
                assert_eq!(snapshot.requests[0].last_by, "Åsa");
                break;
            }
            assert!(Instant::now() < deadline, "the snapshot was not updated");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn deleted_requests_disappear_for_everyone() {
        let (dir_a, dir_b) = (TempDir::new(), TempDir::new());
        let mut a = client(dir_a.path(), "Anna");
        let mut b = client(dir_b.path(), "Bo");
        let id = applied(a.submit(None, &create_action("Upright")));
        let keep = applied(a.submit(None, &create_action("Aero")));
        deliver(dir_a.path(), dir_b.path());
        b.sync();

        // Anna deletes the request. Bo, who has not synced yet, takes it.
        applied(a.submit(
            Some(id),
            &NewAction::Delete {
                note: "Dubblett".to_string(),
            },
        ));
        std::thread::sleep(Duration::from_millis(5));
        applied(b.submit(Some(id), &take_action("Bo")));
        assert_eq!(b.snapshot().requests.len(), 2);

        deliver(dir_a.path(), dir_b.path());
        deliver(dir_b.path(), dir_a.path());
        a.sync();
        let report = b.sync();
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            report.conflicts[0].text,
            "Anna tog bort ärendet före dig, så din ändring gäller inte. Ärendet är borttaget."
        );

        for member in [&a, &b] {
            let snapshot = member.snapshot();
            assert_eq!(snapshot.requests.len(), 1);
            assert_eq!(snapshot.requests[0].request_id, keep);
            assert_eq!(snapshot.deleted.len(), 1);
            assert_eq!(snapshot.deleted[0].title, "Upright");
            assert_eq!(snapshot.deleted[0].by, "Anna");
        }
        assert_eq!(a.state(), b.state());

        // Nothing more can be done with a deleted request.
        match a.submit(Some(id), &take_action("Anna")) {
            Ok(Submitted::Rejected(notice)) => assert!(notice.text.contains("borttaget")),
            other => panic!("unexpected: {other:?}"),
        }
        // The cache keeps the deletion across a rebuild.
        a.rebuild().unwrap();
        assert_eq!(a.snapshot().requests.len(), 1);
    }
}
