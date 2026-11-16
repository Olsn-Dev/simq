//! The egui user interface. Never does file I/O: all of that happens in the background
//! thread (`sync::spawn`) and the interface only reads finished snapshots.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use chrono::{Local, NaiveDate, TimeZone};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod car;
mod overview;
mod stats;

use crate::blobs::{self, FileRef};
use crate::config::{Config, Paths, ThemeChoice};
use crate::github::{GithubRemote, RemoteState, RemoteStatus, RemoteStore};
use crate::i18n::{self, is_en, t, Lang};
use crate::model::{
    char_len, clean_text, is_http_url, list_order, one_line, priority_label, truncate_chars,
    valid_deadline, Department, LocKind, Location, Part, Request, Status, MAX_NAME, MAX_TEXT,
    MAX_TITLE,
};
use crate::ops::NewAction;
use crate::store::SeenState;
use crate::sync::{
    self, Client, Cmd, Event, FileSlot, FolderStatus, Handle, Notice, Snapshot, Source,
};
use crate::theme;
use crate::update;

/// Suggested simulation types (Swedish, English).
const SIM_TYPES: [(&str, &str); 8] = [
    ("FEA / hållfasthet", "FEA / structural"),
    ("CFD / aerodynamik", "CFD / aerodynamics"),
    ("Termisk", "Thermal"),
    ("Modal / vibration", "Modal / vibration"),
    ("Topologioptimering", "Topology optimisation"),
    ("Fordonsdynamik", "Vehicle dynamics"),
    ("Elektromagnetisk", "Electromagnetic"),
    ("Annat", "Other"),
];

/// The three main views of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum View {
    #[default]
    Queue,
    Overview,
    Stats,
}

/// What a finished save should do in the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Close the open form.
    Form,
    /// Empty the comment field.
    Comment,
    Other,
}

/// The filters above the queue. They are local to this app and never synced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Filters {
    /// `Some(None)` means requests without a department.
    department: Option<Option<Uuid>>,
    sim_type: Option<String>,
    priority: Option<u8>,
    /// Requester or assignee.
    person: Option<String>,
    mine: bool,
    unread: bool,
}

impl Filters {
    fn any(&self) -> bool {
        *self != Filters::default()
    }

    fn matches(&self, r: &Request, me: &str, unread: &HashSet<Uuid>) -> bool {
        let is = |name: &str| r.requester == name || r.assignee == name;
        self.department.is_none_or(|d| r.department == d)
            && self.sim_type.as_ref().is_none_or(|t| r.sim_type == *t)
            && self.priority.is_none_or(|p| r.priority == p)
            && self.person.as_ref().is_none_or(|p| is(p))
            && (!self.mine || is(me))
            && (!self.unread || unread.contains(&r.request_id))
    }
}

/// A column the queue can be sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SortCol {
    Seq,
    Title,
    Department,
    Type,
    Priority,
    Status,
    Requester,
    Assignee,
    Deadline,
}

/// What the app remembers between starts, besides the window itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Remembered {
    view: View,
    tab: Tab,
    filters: Filters,
    sort: Option<(SortCol, bool)>,
}

const REMEMBERED_KEY: &str = "simq-view";

/// A message in the corner that goes away by itself.
struct Toast {
    title: String,
    text: String,
    shown: std::time::Instant,
}

const TOAST_SECONDS: f32 = 6.0;

/// A column of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Seq,
    Title,
    Department,
    Type,
    Priority,
    Status,
    Requester,
    Assignee,
    Deadline,
    Links,
}

impl Col {
    /// In the order they are shown.
    const ALL: [Col; 10] = [
        Col::Seq,
        Col::Title,
        Col::Department,
        Col::Type,
        Col::Priority,
        Col::Status,
        Col::Requester,
        Col::Assignee,
        Col::Deadline,
        Col::Links,
    ];

    fn title(self) -> &'static str {
        match self {
            Col::Seq => "#",
            Col::Title => t("Titel", "Title"),
            Col::Department => t("Avdelning", "Department"),
            Col::Type => t("Typ", "Type"),
            Col::Priority => t("Prioritet", "Priority"),
            Col::Status => "Status",
            Col::Requester => t("Beställare", "Requester"),
            Col::Assignee => t("Utförare", "Assignee"),
            Col::Deadline => "Deadline",
            Col::Links => t("Rapport / resultat", "Report / results"),
        }
    }

    /// Width to start with. The title takes what is left over.
    fn width(self) -> f32 {
        match self {
            Col::Seq => 40.0,
            Col::Title => 190.0,
            Col::Priority => 72.0,
            Col::Deadline => 92.0,
            Col::Links => 122.0,
            Col::Status => 108.0,
            _ => 116.0,
        }
    }

    fn sort(self) -> Option<SortCol> {
        Some(match self {
            Col::Seq => SortCol::Seq,
            Col::Title => SortCol::Title,
            Col::Department => SortCol::Department,
            Col::Type => SortCol::Type,
            Col::Priority => SortCol::Priority,
            Col::Status => SortCol::Status,
            Col::Requester => SortCol::Requester,
            Col::Assignee => SortCol::Assignee,
            Col::Deadline => SortCol::Deadline,
            Col::Links => return None,
        })
    }

    /// How soon the column is hidden when the queue gets narrow: the
    /// highest goes first, and 0 is never hidden.
    fn rank(self) -> u8 {
        match self {
            Col::Seq | Col::Title | Col::Status => 0,
            Col::Department => 1,
            Col::Priority => 2,
            Col::Assignee => 3,
            Col::Deadline => 4,
            Col::Type => 5,
            Col::Requester => 6,
            Col::Links => 7,
        }
    }

    /// The columns that fit in the given width, least important hidden first.
    fn fitting(available: f32) -> Vec<Col> {
        let mut columns = Col::ALL.to_vec();
        let needed = |columns: &[Col]| {
            columns.iter().map(|c| c.width()).sum::<f32>() + 9.0 * columns.len() as f32
        };
        while needed(&columns) > available {
            let Some(drop) = columns
                .iter()
                .copied()
                .filter(|c| c.rank() > 0)
                .max_by_key(|c| c.rank())
            else {
                break;
            };
            columns.retain(|c| *c != drop);
        }
        columns
    }
}

/// A question the user has to answer before something irreversible happens.
struct Confirm {
    title: String,
    text: String,
    button: String,
    what: ConfirmWhat,
}

/// How many files were removed and which ones were asked for (SHA-256).
type CleanResult = Result<(usize, Vec<String>), String>;

enum ConfirmWhat {
    Submit(Uuid, Box<NewAction>),
    CleanFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum Tab {
    #[default]
    Active,
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
    All,
}

impl Tab {
    const ALL: [Tab; 7] = [
        Tab::Active,
        Tab::Queued,
        Tab::Running,
        Tab::Done,
        Tab::Failed,
        Tab::Cancelled,
        Tab::All,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Active => t("Aktiva", "Active"),
            Tab::Queued => t("Köade", "Queued"),
            Tab::Running => t("Pågår", "In progress"),
            Tab::Done => t("Klara", "Done"),
            Tab::Failed => t("Misslyckade", "Failed"),
            Tab::Cancelled => t("Avbrutna", "Cancelled"),
            Tab::All => t("Alla", "All"),
        }
    }

    fn matches(self, status: Status) -> bool {
        match self {
            Tab::Active => status.is_active(),
            Tab::Queued => status == Status::Queued,
            Tab::Running => status == Status::Running,
            Tab::Done => status == Status::Done,
            Tab::Failed => status == Status::Failed,
            Tab::Cancelled => status == Status::Cancelled,
            Tab::All => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Settings,
    Diagnostics,
}

/// Input of a location: kind + value.
#[derive(Default)]
struct LocInput {
    kind: LocKind,
    value: String,
    /// The file chosen for upload, filled in by the file picker thread.
    picked: Arc<Mutex<Option<PickedFile>>>,
    picking: Arc<AtomicBool>,
}

#[derive(Clone)]
struct PickedFile {
    path: PathBuf,
    name: String,
    size: u64,
}

impl LocInput {
    fn picked(&self) -> Option<PickedFile> {
        self.picked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The file to upload, if this location is an uploaded file.
    fn file(&self) -> Option<PathBuf> {
        if self.kind == LocKind::File {
            self.picked().map(|picked| picked.path)
        } else {
            None
        }
    }

    fn location(&self) -> Option<Location> {
        let value = self.value.trim();
        if value.is_empty() || self.kind == LocKind::File {
            None
        } else {
            Some(Location {
                kind: self.kind,
                value: value.to_string(),
            })
        }
    }

    fn problem(&self, what: &str, required: bool) -> Option<String> {
        if self.kind == LocKind::File {
            return match self.picked() {
                None => required.then(|| format!("{what} {}", t("saknas", "is missing"))),
                Some(picked) if picked.size > blobs::MAX_FILE_SIZE => Some(format!(
                    "{what}: {}",
                    t(
                        "filen är större än 500 MB",
                        "the file is larger than 500 MB"
                    )
                )),
                Some(picked) if picked.size == 0 => Some(format!(
                    "{what}: {}",
                    t("filen är tom", "the file is empty")
                )),
                Some(_) => None,
            };
        }
        let value = self.value.trim();
        if value.is_empty() {
            return required.then(|| format!("{what} {}", t("saknas", "is missing")));
        }
        if self.kind == LocKind::Drive && !is_http_url(value) {
            return Some(format!(
                "{what}: {}",
                t(
                    "en Google Drive-plats måste vara en länk som börjar med http:// eller https://",
                    "a Google Drive location must be a link starting with http:// or https://"
                )
            ));
        }
        None
    }
}

struct NewForm {
    title: String,
    description: String,
    sim_type: String,
    department: Option<Uuid>,
    part: Option<Uuid>,
    priority: u8,
    requester: String,
    deadline: String,
    input: LocInput,
}

struct FinishForm {
    request_id: Uuid,
    title: String,
    assignee: String,
    report: LocInput,
    results: LocInput,
    summary: String,
}

struct NoteForm {
    request_id: Uuid,
    title: String,
    kind: NoteKind,
    note: String,
}

/// What a note form does when submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteKind {
    Fail,
    Cancel,
    Delete,
}

enum Form {
    New(NewForm),
    Finish(FinishForm),
    Note(NoteForm),
}

impl Form {
    fn window_title(&self) -> String {
        match self {
            Form::New(_) => t("Ny förfrågan", "New request").to_string(),
            Form::Finish(f) => format!(
                "{}: {}",
                t("Markera klar", "Mark as done"),
                truncate_chars(&one_line(&f.title), 40)
            ),
            Form::Note(f) => format!(
                "{}: {}",
                match f.kind {
                    NoteKind::Fail => t("Markera misslyckad", "Mark as failed"),
                    NoteKind::Cancel => t("Avbryt", "Cancel"),
                    NoteKind::Delete => t("Ta bort", "Delete"),
                },
                truncate_chars(&one_line(&f.title), 40)
            ),
        }
    }

    fn submit_label(&self) -> &'static str {
        match self {
            Form::New(_) => t("Lägg i kön", "Add to queue"),
            Form::Finish(_) => t("Markera klar", "Mark as done"),
            Form::Note(f) => match f.kind {
                NoteKind::Fail => t("Markera misslyckad", "Mark as failed"),
                NoteKind::Cancel => t("Avbryt ärendet", "Cancel the request"),
                NoteKind::Delete => t("Ta bort ärendet", "Delete the request"),
            },
        }
    }

    /// The first problem that prevents the form from being submitted.
    fn problem(&self) -> Option<String> {
        match self {
            Form::New(f) => {
                if f.title.trim().is_empty() {
                    Some(t("Titel saknas", "Title is missing").to_string())
                } else if f.requester.trim().is_empty() {
                    Some(t("Beställare saknas", "Requester is missing").to_string())
                } else if !valid_deadline(f.deadline.trim()) {
                    Some(
                        t(
                            "Deadline ska skrivas som ÅÅÅÅ-MM-DD",
                            "Deadline must be written as YYYY-MM-DD",
                        )
                        .to_string(),
                    )
                } else {
                    f.input.problem(t("Indataplats", "Input location"), false)
                }
            }
            Form::Finish(f) => {
                if f.assignee.trim().is_empty() {
                    Some(t("Utförare saknas", "Assignee is missing").to_string())
                } else if let Some(p) = f.report.problem(t("Rapportplats", "Report location"), true)
                {
                    Some(p)
                } else if let Some(p) = f
                    .results
                    .problem(t("Resultatplats", "Results location"), false)
                {
                    Some(p)
                } else if f.summary.trim().is_empty() {
                    Some(t("Sammanfattning saknas", "Summary is missing").to_string())
                } else {
                    None
                }
            }
            Form::Note(_) => None,
        }
    }

    fn action(&self) -> (Option<Uuid>, NewAction, Vec<(FileSlot, PathBuf)>) {
        match self {
            Form::New(f) => (
                None,
                NewAction::Create {
                    title: f.title.trim().to_string(),
                    description: f.description.trim().to_string(),
                    sim_type: f.sim_type.trim().to_string(),
                    department: f.department,
                    part: f.part,
                    priority: f.priority,
                    requester: f.requester.trim().to_string(),
                    deadline: f.deadline.trim().to_string(),
                    input: f.input.location(),
                },
                f.input
                    .file()
                    .map(|path| (FileSlot::Input, path))
                    .into_iter()
                    .collect(),
            ),
            Form::Finish(f) => (
                Some(f.request_id),
                NewAction::Finish {
                    assignee: f.assignee.trim().to_string(),
                    report: f.report.location(),
                    results: f.results.location(),
                    summary: f.summary.trim().to_string(),
                },
                [
                    f.report.file().map(|path| (FileSlot::Report, path)),
                    f.results.file().map(|path| (FileSlot::Results, path)),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
            Form::Note(f) => {
                let note = f.note.trim().to_string();
                let action = match f.kind {
                    NoteKind::Fail => NewAction::Fail { note },
                    NoteKind::Cancel => NewAction::Cancel { note },
                    NoteKind::Delete => NewAction::Delete { note },
                };
                (Some(f.request_id), action, Vec::new())
            }
        }
    }
}

/// Name and sync source, used both at first run and under Settings.
struct IdentityForm {
    name: String,
    folder: Option<PathBuf>,
    /// True = sync via GitHub with a team key, false = shared folder.
    github: bool,
    team_key: String,
    error: Option<String>,
    saved: bool,
}

/// What the user clicked during a frame. Collected and carried out afterwards.
enum UiAction {
    Select(Uuid),
    CloseDetail,
    Take(Uuid),
    Requeue(Uuid),
    OpenFinish(Uuid),
    OpenNote(Uuid, NoteKind),
    OpenUrl(String),
    Download(FileRef),
    Duplicate(Uuid),
    SendComment(Uuid),
    /// Opens the new request form, filled in for a department and maybe a part.
    NewRequest(Option<Uuid>, Option<Uuid>),
    /// Shows the request in the queue.
    GoToRequest(Uuid),
}

/// Where the in-app update stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(update::Available),
    Installing(String),
    /// The new version is in place and takes effect after a restart.
    Installed(String),
    Failed(String),
}

enum UpdateMsg {
    Checked(Result<update::Check, String>),
    Installed(Result<String, String>),
}

pub struct App {
    handle: Handle,
    config: Config,
    paths: Paths,
    ctx: egui::Context,
    snapshot: Arc<Snapshot>,
    view: View,
    tab: Tab,
    filters: Filters,
    sort: Option<(SortCol, bool)>,
    comment_draft: String,
    board: overview::BoardUi,
    confirm: Option<Confirm>,
    pending_kind: Pending,
    /// The request and change that was last reported as seen.
    last_marked: Option<(Uuid, u64)>,
    icons: theme::Icons,
    toasts: Vec<Toast>,
    /// Set by the search shortcut: the search field takes focus next frame.
    focus_search: bool,
    exporting: Option<Receiver<Option<Result<PathBuf, String>>>>,
    cleaning: Option<Receiver<CleanResult>>,
    search: String,
    selected: Option<Uuid>,
    form: Option<Form>,
    form_error: Option<String>,
    pending: Option<u64>,
    identity: IdentityForm,
    identity_pending: Option<u64>,
    settings_open: bool,
    settings_tab: SettingsTab,
    picker: Option<Receiver<Option<PathBuf>>>,
    notices: VecDeque<Notice>,
    next_token: u64,
    was_focused: bool,
    logo: Option<egui::TextureHandle>,
    update: UpdateState,
    update_rx: Option<Receiver<UpdateMsg>>,
    /// True if the user asked for the check (then errors are shown).
    update_manual: bool,
    downloads: Vec<Download>,
}

/// A file being fetched to the user's computer.
struct Download {
    file: FileRef,
    progress: Arc<AtomicU64>,
    rx: Receiver<Option<Result<PathBuf, String>>>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        client: Client,
        config: Config,
        paths: Paths,
    ) -> std::io::Result<App> {
        let ctx = cc.egui_ctx.clone();
        theme::apply(&ctx);
        let logo = theme::load_logo(&ctx);
        let repaint_ctx = ctx.clone();
        let handle = sync::spawn(
            client,
            config.clone(),
            paths.clone(),
            Arc::new(move || repaint_ctx.request_repaint()),
        )?;
        let snapshot = handle.snapshot();
        // If a team key file sits next to the app it is pre-filled at first run.
        let bundled_key = if config.is_complete() {
            None
        } else {
            find_team_key_file()
        };
        let identity = IdentityForm {
            name: config.display_name.clone(),
            folder: config.sync_folder.clone(),
            // GitHub is the default until a shared folder has been chosen.
            github: config.github.is_some() || config.sync_folder.is_none(),
            team_key: bundled_key.unwrap_or_default(),
            error: None,
            saved: false,
        };
        let icons = theme::Icons::load(&ctx);
        let mut app = App {
            handle,
            config,
            paths,
            ctx,
            snapshot,
            view: View::Queue,
            tab: Tab::Active,
            filters: Filters::default(),
            sort: None,
            comment_draft: String::new(),
            board: overview::BoardUi::default(),
            confirm: None,
            pending_kind: Pending::Other,
            last_marked: None,
            icons,
            toasts: Vec::new(),
            focus_search: false,
            exporting: None,
            cleaning: None,
            search: String::new(),
            selected: None,
            form: None,
            form_error: None,
            pending: None,
            identity,
            identity_pending: None,
            settings_open: false,
            settings_tab: SettingsTab::Settings,
            picker: None,
            notices: VecDeque::new(),
            next_token: 1,
            was_focused: false,
            logo,
            update: UpdateState::Idle,
            update_rx: None,
            update_manual: false,
            downloads: Vec::new(),
        };
        app.board.drawing = car::load_drawing(&app.ctx);
        app.board.icons = app.icons.clone();
        // Pick up where the user left off.
        if let Some(saved) = cc
            .storage
            .and_then(|storage| eframe::get_value::<Remembered>(storage, REMEMBERED_KEY))
        {
            app.view = saved.view;
            app.tab = saved.tab;
            app.filters = saved.filters;
            app.sort = saved.sort;
        }
        apply_theme(&app.ctx, app.config.theme);
        // Look for a newer version once at start-up (not from the tests: it uses the network).
        if !cfg!(test) {
            app.check_for_update(false);
        }
        Ok(app)
    }

    fn team_token(&self) -> Option<String> {
        self.config.github.as_ref().map(|g| g.token.clone())
    }

    /// Asks GitHub for a newer version in a background thread.
    fn check_for_update(&mut self, manual: bool) {
        if matches!(
            self.update,
            UpdateState::Checking | UpdateState::Installing(_) | UpdateState::Installed(_)
        ) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let token = self.team_token();
        let spawned = std::thread::Builder::new()
            .name("simq-update".to_string())
            .spawn(move || {
                let _ = tx.send(UpdateMsg::Checked(update::check(token.as_deref())));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.update_rx = Some(rx);
            self.update_manual = manual;
            self.update = UpdateState::Checking;
        }
    }

    /// Downloads and installs a newer version in a background thread.
    fn install_update(&mut self, available: update::Available) {
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let token = self.team_token();
        let version = available.version.clone();
        let spawned = std::thread::Builder::new()
            .name("simq-update".to_string())
            .spawn(move || {
                let result = update::download_and_install(&available, token.as_deref());
                let _ = tx.send(UpdateMsg::Installed(result));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.update_rx = Some(rx);
            self.update_manual = true;
            self.update = UpdateState::Installing(version);
        }
    }

    fn restart_into_update(&mut self) {
        match update::restart() {
            Ok(()) => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(text) => self.notices.push_back(Notice {
                title: t("Kunde inte starta om", "Could not restart").to_string(),
                text: format!(
                    "{} ({text})",
                    t(
                        "Stäng appen och starta den igen för att använda den nya versionen.",
                        "Close the app and start it again to use the new version.",
                    )
                ),
            }),
        }
    }

    /// The update button in the top bar. Only visible when there is something to do.
    fn update_button(&mut self, ui: &mut egui::Ui) {
        match self.update.clone() {
            UpdateState::Available(available) => {
                let label = format!("{} {}", t("Uppdatera till", "Update to"), available.version);
                if theme::add_primary(ui, true, &label).clicked() {
                    self.install_update(available);
                }
            }
            UpdateState::Installing(_) => {
                ui.weak(t("Laddar ner uppdatering…", "Downloading update…"));
                ui.spinner();
            }
            UpdateState::Installed(_) => {
                let clicked = theme::add_primary(ui, true, t("Starta om", "Restart"))
                    .on_hover_text(t(
                        "Den nya versionen är installerad och används efter omstart.",
                        "The new version is installed and is used after a restart.",
                    ))
                    .clicked();
                if clicked {
                    self.restart_into_update();
                }
            }
            _ => {}
        }
    }

    /// Version and update controls under Settings.
    fn update_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.separator();
        ui.label(RichText::new("Version").strong());
        ui.horizontal(|ui| {
            ui.label(format!("SimQ {}", update::current_version()));
            let busy = matches!(
                self.update,
                UpdateState::Checking | UpdateState::Installing(_)
            );
            if !matches!(self.update, UpdateState::Installed(_))
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(t("Sök efter uppdatering", "Check for updates")),
                    )
                    .clicked()
            {
                self.check_for_update(true);
            }
            if busy {
                ui.spinner();
            }
        });
        match self.update.clone() {
            UpdateState::Idle => {}
            UpdateState::Checking => {
                ui.weak(t("Söker…", "Checking…"));
            }
            UpdateState::UpToDate => {
                ui.weak(t(
                    "Du har den senaste versionen.",
                    "You have the latest version.",
                ));
            }
            UpdateState::Available(available) => {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Version {} {}",
                        available.version,
                        t("finns.", "is available.")
                    ));
                    if theme::add_primary(ui, true, t("Uppdatera", "Update")).clicked() {
                        self.install_update(available);
                    }
                });
            }
            UpdateState::Installing(version) => {
                ui.weak(format!(
                    "{} {version}…",
                    t("Laddar ner version", "Downloading version")
                ));
            }
            UpdateState::Installed(version) => {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Version {version} {}",
                        t("är installerad.", "is installed.")
                    ));
                    if theme::add_primary(ui, true, t("Starta om", "Restart")).clicked() {
                        self.restart_into_update();
                    }
                });
            }
            UpdateState::Failed(text) => {
                ui.colored_label(ui.visuals().error_fg_color, text);
            }
        }
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn can_write(&self) -> bool {
        self.snapshot.status == FolderStatus::Ok && self.pending.is_none()
    }

    fn why_read_only(&self) -> &'static str {
        match self.snapshot.status {
            FolderStatus::Ok => t("Sparar…", "Saving…"),
            FolderStatus::NewerFormat => t(
                "Mappen kräver en nyare version. Uppdatera appen.",
                "The folder requires a newer version. Update the app.",
            ),
            FolderStatus::Starting => t("Väntar på första synken…", "Waiting for the first sync…"),
            _ => t(
                "Mappen går inte att nå just nu, så inget kan ändras.",
                "The folder cannot be reached right now, so nothing can be changed.",
            ),
        }
    }

    fn submit(
        &mut self,
        request_id: Option<Uuid>,
        action: NewAction,
        files: Vec<(FileSlot, PathBuf)>,
        kind: Pending,
    ) {
        let token = self.token();
        self.pending = Some(token);
        self.pending_kind = kind;
        self.form_error = None;
        self.handle.send(Cmd::Submit {
            token,
            request_id,
            action,
            files,
        });
    }

    /// Saves several events in one go, e.g. the suggested departments.
    fn submit_many(&mut self, items: Vec<(Uuid, NewAction)>) {
        let token = self.token();
        self.pending = Some(token);
        self.pending_kind = Pending::Other;
        self.handle.send(Cmd::SubmitMany { token, items });
    }

    /// The requests that pass the search and the filters, and the tab if one is given.
    fn filtered<'a>(&self, snapshot: &'a Snapshot, tab: Option<Tab>) -> Vec<&'a Request> {
        let needle = self.search.trim().to_lowercase();
        snapshot
            .requests
            .iter()
            .filter(|r| {
                tab.is_none_or(|tab| tab.matches(r.status))
                    && matches_search(r, &needle, snapshot)
                    && self
                        .filters
                        .matches(r, &self.config.display_name, &snapshot.unread)
            })
            .collect()
    }

    /// Asks where to save the list as CSV and writes it in a background thread.
    fn export_csv(&mut self) {
        if self.exporting.is_some() {
            return;
        }
        let snapshot = Arc::clone(&self.snapshot);
        let mut rows = self.filtered(&snapshot, Some(self.tab));
        self.sort_rows(&mut rows, &snapshot);
        let text = crate::export::requests_csv(&rows, &snapshot.departments, &snapshot.parts);
        let name = format!("simq-{}.csv", Local::now().format("%Y-%m-%d"));
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("simq-export".to_string())
            .spawn(move || {
                let dest = rfd::FileDialog::new()
                    .set_title(t("Spara listan som CSV", "Save the list as CSV"))
                    .set_file_name(name)
                    .save_file();
                let result = dest.map(|dest| {
                    std::fs::write(&dest, text.as_bytes())
                        .map(|()| dest)
                        .map_err(|e| e.to_string())
                });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.exporting = Some(rx);
        }
    }

    /// Removes uploaded files that only deleted requests refer to.
    fn clean_files(&mut self) {
        let Some(folder) = self.config.effective_folder(&self.paths) else {
            return;
        };
        if self.cleaning.is_some() {
            return;
        }
        let files = self.snapshot.orphan_files.clone();
        let github = self.config.github.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("simq-clean".to_string())
            .spawn(move || {
                let mut remote = github.map(|config| GithubRemote::new(&config));
                let result = blobs::remove(
                    &folder,
                    remote.as_mut().map(|r| r as &mut dyn RemoteStore),
                    &files,
                )
                .map(|removed| (removed, files.into_iter().map(|f| f.sha256).collect()));
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.cleaning = Some(rx);
        }
    }

    fn sort_rows(&self, rows: &mut [&Request], snapshot: &Snapshot) {
        match self.sort {
            None => rows.sort_by(|a, b| list_order(a, b)),
            Some((col, ascending)) => rows.sort_by(|a, b| {
                let order = sort_cmp(col, a, b, snapshot).then(a.seq.cmp(&b.seq));
                if ascending {
                    order
                } else {
                    order.reverse()
                }
            }),
        }
    }

    /// Asks where to save an uploaded file and fetches it in a background thread.
    fn start_download(&mut self, file: FileRef) {
        let Some(folder) = self.config.effective_folder(&self.paths) else {
            return;
        };
        let github = self.config.github.clone();
        let progress = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let (thread_file, thread_progress) = (file.clone(), Arc::clone(&progress));
        let spawned = std::thread::Builder::new()
            .name("simq-download".to_string())
            .spawn(move || {
                let dest = rfd::FileDialog::new()
                    .set_title(t("Spara filen", "Save the file"))
                    .set_file_name(&thread_file.name)
                    .save_file();
                let result = dest.map(|dest| {
                    let mut remote = github.map(|config| GithubRemote::new(&config));
                    blobs::fetch(
                        &folder,
                        remote.as_mut().map(|r| r as &mut dyn RemoteStore),
                        &thread_file,
                        &dest,
                        &thread_progress,
                    )
                    .map(|()| dest)
                });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.downloads.push(Download { file, progress, rx });
        }
    }

    /// Collects finished downloads and tells the user how they went.
    fn poll_downloads(&mut self, ctx: &egui::Context) {
        let mut still_running = Vec::new();
        for download in std::mem::take(&mut self.downloads) {
            match download.rx.try_recv() {
                Ok(Some(Ok(dest))) => self.toast(
                    t("Filen är sparad", "The file is saved"),
                    dest.display().to_string(),
                ),
                Ok(Some(Err(text))) => self.notices.push_back(Notice {
                    title: t("Filen kunde inte hämtas", "The file could not be fetched")
                        .to_string(),
                    text,
                }),
                // The save dialog was cancelled, or the thread went away.
                Ok(None) | Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => still_running.push(download),
            }
        }
        self.downloads = still_running;
        if !self.downloads.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// Receives replies from the background thread and the latest snapshot.
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.handle.events.try_recv() {
            match event {
                Event::Notice(notice) => self.notices.push_back(notice),
                Event::SubmitDone { token, result } => {
                    if self.pending != Some(token) {
                        continue;
                    }
                    self.pending = None;
                    let kind = self.pending_kind;
                    match result {
                        Ok(()) => match kind {
                            Pending::Form => {
                                self.form = None;
                                self.form_error = None;
                            }
                            Pending::Comment => self.comment_draft.clear(),
                            Pending::Other => {}
                        },
                        Err(text) if kind == Pending::Form && self.form.is_some() => {
                            self.form_error = Some(text)
                        }
                        Err(text) => self.notices.push_back(Notice {
                            title: t("Det gick inte att spara", "Could not save").to_string(),
                            text,
                        }),
                    }
                }
                Event::ConfigureDone { token, result } => {
                    if self.identity_pending != Some(token) {
                        continue;
                    }
                    self.identity_pending = None;
                    match result {
                        Ok(config) => {
                            let repo = |c: &Config| c.github.as_ref().map(|g| g.repo.clone());
                            if self.config.sync_folder != config.sync_folder
                                || repo(&self.config) != repo(&config)
                            {
                                self.selected = None;
                                self.form = None;
                            }
                            self.identity.name = config.display_name.clone();
                            self.identity.folder = config.sync_folder.clone();
                            self.identity.github = config.github.is_some();
                            self.identity.team_key.clear();
                            self.identity.error = None;
                            self.identity.saved = true;
                            self.config = config;
                        }
                        Err(text) => self.identity.error = Some(text),
                    }
                }
            }
        }
        self.snapshot = self.handle.snapshot();
        self.poll_downloads(ctx);

        if let Some(rx) = &self.exporting {
            match rx.try_recv() {
                Ok(result) => {
                    self.exporting = None;
                    match result {
                        Some(Ok(dest)) => self.toast(
                            t("Listan är sparad", "The list is saved"),
                            dest.display().to_string(),
                        ),
                        Some(Err(text)) => self.notices.push_back(Notice {
                            title: t("Listan kunde inte sparas", "The list could not be saved")
                                .to_string(),
                            text,
                        }),
                        None => {}
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.exporting = None,
            }
        }
        if let Some(rx) = &self.cleaning {
            match rx.try_recv() {
                Ok(result) => {
                    self.cleaning = None;
                    match result {
                        Ok((removed, sha256)) => {
                            self.handle.send(Cmd::FilesCleaned(sha256));
                            self.toast(
                                t("Filerna är rensade", "The files are cleaned up"),
                                if is_en() {
                                    format!("{removed} file(s) were removed.")
                                } else {
                                    format!("{removed} fil(er) togs bort.")
                                },
                            );
                        }
                        Err(text) => self.notices.push_back(Notice {
                            title: t(
                                "Filerna kunde inte rensas",
                                "The files could not be cleaned up",
                            )
                            .to_string(),
                            text,
                        }),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.cleaning = None,
            }
        }

        if let Some(rx) = &self.update_rx {
            match rx.try_recv() {
                Ok(UpdateMsg::Checked(result)) => {
                    self.update_rx = None;
                    self.update = match result {
                        Ok(update::Check::UpToDate) => UpdateState::UpToDate,
                        Ok(update::Check::Available(available)) => {
                            UpdateState::Available(available)
                        }
                        // A failed automatic check at start-up is not worth a message.
                        Err(text) if self.update_manual => UpdateState::Failed(text),
                        Err(_) => UpdateState::Idle,
                    };
                }
                Ok(UpdateMsg::Installed(result)) => {
                    self.update_rx = None;
                    self.update = match result {
                        Ok(version) => UpdateState::Installed(version),
                        Err(text) => {
                            self.notices.push_back(Notice {
                                title: t("Uppdateringen misslyckades", "The update failed")
                                    .to_string(),
                                text: text.clone(),
                            });
                            UpdateState::Failed(text)
                        }
                    };
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.update_rx = None;
                    self.update = UpdateState::Idle;
                }
            }
        }

        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            self.handle.send(Cmd::FocusSync);
        }
        self.was_focused = focused;

        if let Some(rx) = &self.picker {
            match rx.try_recv() {
                Ok(choice) => {
                    if let Some(folder) = choice {
                        self.identity.folder = Some(folder);
                        self.identity.error = None;
                        self.identity.saved = false;
                    }
                    self.picker = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.picker = None,
            }
        }
    }

    /// Opens the system folder picker in its own thread so the interface does not freeze.
    fn pick_folder(&mut self) {
        if self.picker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let start = self.identity.folder.clone();
        let spawned = std::thread::Builder::new()
            .name("simq-folder-picker".to_string())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title(t("Välj den delade mappen", "Choose the shared folder"));
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                let _ = tx.send(dialog.pick_folder());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.picker = Some(rx);
        }
    }

    /// Changes between light and dark mode. The background thread saves the choice.
    fn set_theme(&mut self, theme: ThemeChoice) {
        apply_theme(&self.ctx, theme);
        self.config.theme = theme;
        self.handle.send(Cmd::SetTheme(theme));
    }

    /// Changes language immediately in the interface. The background thread saves the choice.
    fn set_language(&mut self, lang: Lang) {
        i18n::set(lang);
        self.config.language = lang;
        self.handle.send(Cmd::SetLanguage(lang));
    }

    fn save_identity(&mut self) {
        let source = if self.identity.github {
            let key = self.identity.team_key.trim();
            if key.is_empty() {
                Source::Keep
            } else {
                Source::TeamKey(key.to_string())
            }
        } else {
            match self.identity.folder.clone() {
                Some(folder) => Source::Folder(folder),
                None => return,
            }
        };
        let token = self.token();
        self.identity_pending = Some(token);
        self.identity.error = None;
        self.identity.saved = false;
        self.handle.send(Cmd::Configure {
            token,
            display_name: self.identity.name.clone(),
            source,
        });
    }

    /// The fields for name and sync source. Returns true if Save/Start was clicked.
    fn identity_fields(&mut self, ui: &mut egui::Ui, button: &str) -> bool {
        let busy = self.identity_pending.is_some();
        ui.label(RichText::new("Språk / Language").strong());
        ui.horizontal(|ui| {
            for lang in Lang::ALL {
                let current = self.config.language == lang;
                if ui.selectable_label(current, lang.name()).clicked() && !current {
                    self.set_language(lang);
                }
            }
        });
        ui.add_space(10.0);

        ui.label(RichText::new(t("Utseende", "Appearance")).strong());
        ui.horizontal(|ui| {
            for theme in ThemeChoice::ALL {
                let current = self.config.theme == theme;
                if ui.selectable_label(current, theme.label()).clicked() && !current {
                    self.set_theme(theme);
                }
            }
        });
        ui.add_space(10.0);

        ui.label(RichText::new(t("Visningsnamn", "Display name")).strong());
        ui.add(
            egui::TextEdit::singleline(&mut self.identity.name)
                .hint_text(t("t.ex. Anna Andersson", "e.g. Anna Andersson"))
                .char_limit(MAX_NAME)
                .desired_width(f32::INFINITY),
        );
        ui.weak(t(
            "Namnet visas för laget som beställare och utförare.",
            "The name is shown to the team as requester and assignee.",
        ));
        ui.add_space(10.0);

        ui.label(RichText::new(t("Synka via", "Sync via")).strong());
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.identity.github, "GitHub")
                .clicked()
            {
                self.identity.github = true;
                self.identity.saved = false;
            }
            if ui
                .selectable_label(!self.identity.github, t("Delad mapp", "Shared folder"))
                .clicked()
            {
                self.identity.github = false;
                self.identity.saved = false;
            }
        });
        ui.add_space(6.0);

        if self.identity.github {
            ui.label(RichText::new(t("Teamnyckel", "Team key")).strong());
            let connected = self.config.github.as_ref().map(|g| g.repo.clone());
            if let Some(repo) = &connected {
                ui.label(format!(
                    "{} {}",
                    t("Ansluten till", "Connected to"),
                    one_line(repo)
                ));
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.identity.team_key)
                    .password(true)
                    .hint_text(if connected.is_some() {
                        t(
                            "lämna tomt för att behålla nyckeln",
                            "leave empty to keep the key",
                        )
                    } else {
                        t("ägare/repo:token", "owner/repo:token")
                    })
                    .char_limit(600)
                    .desired_width(f32::INFINITY),
            );
            ui.weak(t(
                "Klistra in teamnyckeln du fått av laget. Appen synkar då mot lagets privata \
                 GitHub-repo. Du behöver inget eget GitHub-konto.",
                "Paste the team key you got from the team. The app then syncs with the team's \
                 private GitHub repository. You do not need a GitHub account of your own.",
            ));
            ui.add_space(10.0);
        } else {
            ui.label(RichText::new(t("Delad mapp", "Shared folder")).strong());
            match &self.identity.folder {
                Some(folder) => {
                    ui.label(folder.display().to_string());
                }
                None => {
                    ui.weak(t("Ingen mapp vald", "No folder selected"));
                }
            }
            if ui
                .add_enabled(
                    self.picker.is_none() && !busy,
                    egui::Button::new(t("Välj mapp…", "Choose folder…")),
                )
                .clicked()
            {
                self.pick_folder();
            }
            ui.weak(t(
                "Välj lagets mapp (t.ex. \"SimQ\") i Google Drive för skrivbord. \
                     Appen läser och skriver bara vanliga filer i den mappen.",
                "Choose the team's folder (e.g. \"SimQ\") in Google Drive for desktop. \
                     The app only reads and writes ordinary files in that folder.",
            ));
            ui.add_space(10.0);
        }

        if let Some(error) = &self.identity.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let mut clicked = false;
        ui.horizontal(|ui| {
            let source_ready = if self.identity.github {
                !self.identity.team_key.trim().is_empty() || self.config.github.is_some()
            } else {
                self.identity.folder.is_some()
            };
            let ready = !busy && !self.identity.name.trim().is_empty() && source_ready;
            clicked = theme::add_primary(ui, ready, button)
                .on_disabled_hover_text(t(
                    "Fyll i visningsnamn och teamnyckel (eller välj en mapp)",
                    "Enter a display name and a team key (or choose a folder)",
                ))
                .clicked();
            if busy {
                ui.spinner();
                ui.weak(t("Kontrollerar…", "Checking…"));
            } else if self.identity.saved {
                ui.weak(t("Sparat", "Saved"));
            }
        });
        clicked
    }

    // -----------------------------------------------------------------------
    // First run
    // -----------------------------------------------------------------------

    fn setup_ui(&mut self, ctx: &egui::Context) {
        let logo = self.logo.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.06).max(12.0));
                    theme::logo(ui, logo.as_ref(), 84.0);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(t("Välkommen till SimQ", "Welcome to SimQ"))
                            .size(26.0)
                            .strong(),
                    );
                    ui.weak(t(
                        "Lagets kö för simuleringsförfrågor.",
                        "The team's queue for simulation requests.",
                    ));
                    ui.add_space(18.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(484.0, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::Frame::new()
                                .fill(ui.visuals().window_fill)
                                .stroke(ui.visuals().window_stroke)
                                .corner_radius(egui::CornerRadius::same(12))
                                .inner_margin(egui::Margin::same(22))
                                .show(ui, |ui| {
                                    ui.set_width(440.0);
                                    if self.identity_fields(ui, t("Starta", "Start")) {
                                        self.save_identity();
                                    }
                                });
                        },
                    );
                    ui.add_space(20.0);
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Main view
    // -----------------------------------------------------------------------

    fn top_ui(&mut self, ctx: &egui::Context) {
        let snapshot = Arc::clone(&self.snapshot);
        let can_write = self.can_write();
        let logo = self.logo.clone();
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                theme::finish_line(ui);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    theme::logo(ui, logo.as_ref(), 26.0);
                    ui.label(
                        RichText::new("SimQ")
                            .font(theme::semibold_font(21.0))
                            .color(ui.visuals().strong_text_color()),
                    );
                    ui.add_space(10.0);
                    for (view, label, key) in [
                        (View::Queue, t("Kö", "Queue"), egui::Key::Num1),
                        (View::Overview, t("Översikt", "Overview"), egui::Key::Num2),
                        (View::Stats, t("Statistik", "Statistics"), egui::Key::Num3),
                    ] {
                        if theme::nav_tab(ui, self.view == view, label)
                            .on_hover_text(shortcut_text(ui.ctx(), key))
                            .clicked()
                        {
                            self.view = view;
                        }
                    }
                    ui.add_space(10.0);
                    if theme::add_primary_icon(
                        ui,
                        can_write,
                        &self.icons,
                        theme::Icon::Plus,
                        t("Ny förfrågan", "New request"),
                    )
                    .on_hover_text(shortcut_text(ui.ctx(), egui::Key::N))
                    .on_disabled_hover_text(self.why_read_only())
                    .clicked()
                    {
                        self.open_new_form(None, None);
                    }
                    if self.view == View::Queue {
                        ui.add_space(8.0);
                        self.icons.show(ui, theme::Icon::Search, 14.0);
                        let search = ui
                            .add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text(t(
                                        "Sök titel, beställare, typ, avdelning",
                                        "Search title, requester, type, department",
                                    ))
                                    .desired_width(220.0),
                            )
                            .on_hover_text(shortcut_text(ui.ctx(), egui::Key::F));
                        if std::mem::take(&mut self.focus_search) {
                            search.request_focus();
                        }
                        if !self.search.is_empty()
                            && ui
                                .add(self.icons.button(ui, theme::Icon::Close, "", "×").small())
                                .on_hover_text(t("Rensa sökningen", "Clear the search"))
                                .clicked()
                        {
                            self.search.clear();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(self.icons.button(
                                ui,
                                theme::Icon::Gear,
                                t("Inställningar", "Settings"),
                                "",
                            ))
                            .clicked()
                        {
                            self.settings_open = true;
                            self.identity.saved = false;
                        }
                        // Shows what a click switches to.
                        let dark = ui.visuals().dark_mode;
                        let (icon, fallback) = if dark {
                            (theme::Icon::Sun, "☀")
                        } else {
                            (theme::Icon::Moon, "🌙")
                        };
                        if ui
                            .add(self.icons.button(ui, icon, "", fallback))
                            .on_hover_text(if dark {
                                t("Byt till ljust läge", "Switch to light mode")
                            } else {
                                t("Byt till mörkt läge", "Switch to dark mode")
                            })
                            .clicked()
                        {
                            self.set_theme(if dark {
                                ThemeChoice::Light
                            } else {
                                ThemeChoice::Dark
                            });
                        }
                        if ui
                            .add(self.icons.button(
                                ui,
                                theme::Icon::Sync,
                                t("Synka nu", "Sync now"),
                                "",
                            ))
                            .clicked()
                        {
                            self.handle.send(Cmd::SyncNow);
                        }
                        self.update_button(ui);
                    });
                });
                ui.add_space(4.0);
                if self.view != View::Queue {
                    return;
                }
                ui.add_space(6.0);
                let base = self.filtered(&snapshot, None);
                ui.horizontal_wrapped(|ui| {
                    for tab in Tab::ALL {
                        let count = base.iter().filter(|r| tab.matches(r.status)).count();
                        ui.selectable_value(
                            &mut self.tab,
                            tab,
                            format!("{}  {count}", tab.label()),
                        );
                    }
                });
                ui.add_space(2.0);
                self.filter_ui(ui, &snapshot);
            });
    }

    /// Opens the form for a new request, optionally filled in for a department and a part.
    fn open_new_form(&mut self, department: Option<Uuid>, part: Option<Uuid>) {
        self.form_error = None;
        self.form = Some(Form::New(NewForm {
            title: String::new(),
            description: String::new(),
            sim_type: String::new(),
            department,
            part,
            priority: 2,
            requester: self.config.display_name.clone(),
            deadline: String::new(),
            input: LocInput::default(),
        }));
    }

    /// The row of filters above the queue.
    fn filter_ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let all = t("Alla", "All");
        let mut types: Vec<&str> = snapshot
            .requests
            .iter()
            .map(|r| r.sim_type.as_str())
            .filter(|t| !t.trim().is_empty())
            .collect();
        types.sort_unstable();
        types.dedup();
        let mut people: Vec<&str> = snapshot
            .requests
            .iter()
            .flat_map(|r| [r.requester.as_str(), r.assignee.as_str()])
            .filter(|p| !p.trim().is_empty())
            .collect();
        people.sort_unstable();
        people.dedup();

        ui.horizontal_wrapped(|ui| {
            ui.weak(t("Filter:", "Filter:"));
            let department = match self.filters.department {
                None => all.to_string(),
                Some(None) => t("Utan avdelning", "No department").to_string(),
                Some(Some(id)) => department_name(snapshot, Some(id)),
            };
            egui::ComboBox::from_id_salt("filter_department")
                .selected_text(format!("{}: {department}", t("Avdelning", "Department")))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.department, None, all);
                    for d in &snapshot.departments {
                        ui.selectable_value(
                            &mut self.filters.department,
                            Some(Some(d.id)),
                            truncate_chars(&one_line(&d.name), 40),
                        );
                    }
                    ui.selectable_value(
                        &mut self.filters.department,
                        Some(None),
                        t("Utan avdelning", "No department"),
                    );
                });
            egui::ComboBox::from_id_salt("filter_type")
                .selected_text(format!(
                    "{}: {}",
                    t("Typ", "Type"),
                    truncate_chars(
                        &one_line(self.filters.sim_type.as_deref().unwrap_or(all)),
                        30
                    )
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.sim_type, None, all);
                    for sim_type in &types {
                        ui.selectable_value(
                            &mut self.filters.sim_type,
                            Some(sim_type.to_string()),
                            truncate_chars(&one_line(sim_type), 40),
                        );
                    }
                });
            egui::ComboBox::from_id_salt("filter_priority")
                .selected_text(format!(
                    "{}: {}",
                    t("Prioritet", "Priority"),
                    self.filters.priority.map(priority_label).unwrap_or(all)
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.priority, None, all);
                    for priority in 1..=3u8 {
                        ui.selectable_value(
                            &mut self.filters.priority,
                            Some(priority),
                            priority_label(priority),
                        );
                    }
                });
            egui::ComboBox::from_id_salt("filter_person")
                .selected_text(format!(
                    "{}: {}",
                    t("Person", "Person"),
                    truncate_chars(&one_line(self.filters.person.as_deref().unwrap_or(all)), 30)
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.person, None, all);
                    for person in &people {
                        ui.selectable_value(
                            &mut self.filters.person,
                            Some(person.to_string()),
                            truncate_chars(&one_line(person), 40),
                        );
                    }
                });
            ui.toggle_value(&mut self.filters.mine, t("Mina", "Mine"))
                .on_hover_text(t(
                    "Bara ärenden där du är beställare eller utförare",
                    "Only requests where you are the requester or the assignee",
                ));
            let unread = snapshot.unread.len();
            let label = if unread > 0 {
                format!("{} {unread}", t("Nytt för dig", "New for you"))
            } else {
                t("Nytt för dig", "New for you").to_string()
            };
            ui.toggle_value(&mut self.filters.unread, label)
                .on_hover_text(t(
                    "Dina ärenden som någon annan har ändrat sedan du senast öppnade dem",
                    "Your requests that someone else has changed since you last opened them",
                ));
            if self.filters.any()
                && ui
                    .small_button(t("Rensa filter", "Clear filters"))
                    .clicked()
            {
                self.filters = Filters::default();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.exporting.is_none(),
                        self.icons
                            .button(
                                ui,
                                theme::Icon::Export,
                                t("Exportera CSV", "Export CSV"),
                                "",
                            )
                            .small(),
                    )
                    .on_hover_text(t(
                        "Sparar listan som den visas nu, till exempel för Excel",
                        "Saves the list as it is shown now, for example for Excel",
                    ))
                    .clicked()
                {
                    self.export_csv();
                }
            });
        });
    }

    fn status_ui(&mut self, ctx: &egui::Context) {
        let snapshot = Arc::clone(&self.snapshot);
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let ok_color = theme::status_color(Status::Done, ui.visuals().dark_mode);
                    let idle_color = ui.visuals().weak_text_color();
                    let warn_color = ui.visuals().warn_fg_color;
                    let error_color = ui.visuals().error_fg_color;
                    let local_ok = matches!(
                        snapshot.status,
                        FolderStatus::Ok | FolderStatus::Starting | FolderStatus::Missing
                    );
                    if let (Some(remote), true) = (&snapshot.remote, local_ok) {
                        remote_status_ui(ui, remote);
                    } else {
                        let (color, text) = match &snapshot.status {
                            FolderStatus::Ok => match snapshot.last_sync_ms {
                                Some(ms) => (
                                    ok_color,
                                    format!("{} {}", t("Synkad", "Synced"), format_clock(ms)),
                                ),
                                None => (idle_color, t("Synkar…", "Syncing…").to_string()),
                            },
                            FolderStatus::Starting => {
                                (idle_color, t("Synkar…", "Syncing…").to_string())
                            }
                            FolderStatus::NotConfigured => (
                                idle_color,
                                t("Ingen mapp vald", "No folder selected").to_string(),
                            ),
                            FolderStatus::Missing => (
                                warn_color,
                                t(
                                    "Mappen hittas inte, visar sparad data",
                                    "Folder not found, showing saved data",
                                )
                                .to_string(),
                            ),
                            FolderStatus::NewerFormat => (
                                warn_color,
                                t(
                                    "Mappen kräver en nyare version. Uppdatera appen för att kunna göra ändringar.",
                                    "The folder requires a newer version. Update the app to make changes.",
                                )
                                .to_string(),
                            ),
                            FolderStatus::Error(text) => (error_color, one_line(text)),
                        };
                        theme::dot(ui, color);
                        ui.label(text);
                    }
                    if let Some(download) = self.downloads.first() {
                        let done = download.progress.load(Ordering::Relaxed);
                        let percent = done.saturating_mul(100) / download.file.size.max(1);
                        ui.separator();
                        ui.spinner();
                        ui.label(format!(
                            "{} {}… {percent} %",
                            t("Hämtar", "Fetching"),
                            truncate_chars(&one_line(&download.file.name), 40)
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(one_line(&self.config.display_name));
                        if snapshot.unsupported_seen {
                            ui.separator();
                            ui.colored_label(
                                warn_color,
                                t(
                                    "Ny version av appen finns",
                                    "A new version of the app is available",
                                ),
                            );
                        }
                    });
                });
            });
    }

    fn table_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        let snapshot = Arc::clone(&self.snapshot);
        let filtering = !self.search.trim().is_empty() || self.filters.any();
        let selected = self.selected;
        let mut rows = self.filtered(&snapshot, Some(self.tab));
        self.sort_rows(&mut rows, &snapshot);
        let sort = self.sort;
        let can_write = self.can_write();
        let icons = self.icons.clone();
        let mut clicked_header = None;
        let (mut new_request, mut clear_filters, mut show_all) = (false, false, false);
        let today = Local::now().date_naive();

        egui::CentralPanel::default().show(ctx, |ui| {
            if rows.is_empty() {
                ui.add_space((ui.available_height() * 0.22).max(24.0));
                ui.vertical_centered(|ui| {
                    if snapshot.requests.is_empty() {
                        empty_state(
                            ui,
                            t("Kön är tom", "The queue is empty"),
                            t(
                                "Lägg upp lagets första simuleringsförfrågan.",
                                "Add the team's first simulation request.",
                            ),
                        );
                        new_request = theme::add_primary_icon(
                            ui,
                            can_write,
                            &icons,
                            theme::Icon::Plus,
                            t("Ny förfrågan", "New request"),
                        )
                        .clicked();
                    } else if filtering {
                        empty_state(
                            ui,
                            t("Inga träffar", "No matches"),
                            t(
                                "Inga ärenden matchar sökningen och filtren.",
                                "No requests match the search and the filters.",
                            ),
                        );
                        clear_filters = ui
                            .button(t("Rensa sökning och filter", "Clear search and filters"))
                            .clicked();
                    } else {
                        empty_state(
                            ui,
                            t("Inget här just nu", "Nothing here right now"),
                            t(
                                "Inga ärenden under den här fliken.",
                                "No requests under this tab.",
                            ),
                        );
                        show_all = ui
                            .button(t("Visa alla ärenden", "Show all requests"))
                            .clicked();
                    }
                });
                return;
            }
            // A click should select the row, not select text.
            ui.style_mut().interaction.selectable_labels = false;
            let overdue_color = ui.visuals().error_fg_color;
            let dark = ui.visuals().dark_mode;
            // Only the columns that fit: with the request open beside the
            // queue the least important ones are hidden instead of cut off.
            let columns = Col::fitting(ui.available_width());
            let mut table = TableBuilder::new(ui)
                .id_salt(("queue", columns.len()))
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for col in &columns {
                table = table.column(if *col == Col::Title {
                    Column::remainder().at_least(150.0).clip(true)
                } else {
                    Column::initial(col.width()).at_least(30.0).clip(true)
                });
            }
            table
                .header(26.0, |mut header| {
                    for col in &columns {
                        header.col(|ui| {
                            let text = RichText::new(col.title().to_uppercase())
                                .font(theme::semibold_font(11.5))
                                .color(ui.visuals().weak_text_color());
                            let Some(by) = col.sort() else {
                                ui.label(text);
                                return;
                            };
                            let response = ui
                                .add(egui::Label::new(text).sense(egui::Sense::click()))
                                .on_hover_text(t("Klicka för att sortera", "Click to sort"));
                            if response.clicked() {
                                clicked_header = Some(by);
                            }
                            if let Some((_, ascending)) = sort.filter(|(sorted, _)| *sorted == by) {
                                // A small arrow: up for ascending.
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                let (tip, base) = if ascending {
                                    (rect.top() + 1.0, rect.bottom() - 1.0)
                                } else {
                                    (rect.bottom() - 1.0, rect.top() + 1.0)
                                };
                                ui.painter().add(egui::Shape::convex_polygon(
                                    vec![
                                        egui::pos2(rect.center().x, tip),
                                        egui::pos2(rect.right(), base),
                                        egui::pos2(rect.left(), base),
                                    ],
                                    theme::ACCENT,
                                    egui::Stroke::NONE,
                                ));
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let request = rows[row.index()];
                        row.set_selected(selected == Some(request.request_id));
                        for col in &columns {
                            row.col(|ui| match col {
                                Col::Seq => {
                                    ui.label(
                                        RichText::new(request.seq.to_string())
                                            .font(theme::semibold_font(13.0))
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                Col::Title => {
                                    if snapshot.unread.contains(&request.request_id) {
                                        theme::dot(ui, theme::ACCENT);
                                    }
                                    cell(ui, &request.title);
                                    if !request.comments.is_empty() {
                                        ui.spacing_mut().item_spacing.x = 3.0;
                                        icons.show(ui, theme::Icon::Comment, 12.0);
                                        ui.weak(request.comments.len().to_string()).on_hover_text(
                                            t("Antal kommentarer", "Number of comments"),
                                        );
                                    }
                                }
                                Col::Department => {
                                    if let Some(d) = find_department(&snapshot, request.department)
                                    {
                                        theme::dot(ui, theme::dept_color(d.color, dark));
                                        cell(ui, &d.name);
                                    }
                                }
                                Col::Type => cell(ui, &request.sim_type),
                                Col::Priority => {
                                    let text = RichText::new(priority_label(request.priority))
                                        .color(theme::priority_color(request.priority, ui));
                                    ui.label(if request.priority == 1 {
                                        text.strong()
                                    } else {
                                        text
                                    });
                                }
                                Col::Status => {
                                    theme::status_pill(ui, request.status);
                                }
                                Col::Requester => cell(ui, &request.requester),
                                Col::Assignee => cell(ui, &request.assignee),
                                Col::Deadline => {
                                    if is_overdue(request, today) {
                                        ui.colored_label(overdue_color, &request.deadline)
                                            .on_hover_text(t(
                                                "Deadline har passerat",
                                                "The deadline has passed",
                                            ));
                                    } else {
                                        ui.label(&request.deadline);
                                    }
                                }
                                Col::Links => {
                                    if request.status == Status::Done {
                                        quick_link(
                                            ui,
                                            t("Rapport", "Report"),
                                            request.report.as_ref(),
                                            actions,
                                        );
                                        quick_link(
                                            ui,
                                            t("Resultat", "Results"),
                                            request.results.as_ref(),
                                            actions,
                                        );
                                    }
                                }
                            });
                        }
                        if row.response().clicked() {
                            actions.push(UiAction::Select(request.request_id));
                        }
                    });
                });
        });
        // A click on a header sorts ascending, then descending, then back to the usual order.
        if let Some(col) = clicked_header {
            self.sort = match self.sort {
                Some((by, true)) if by == col => Some((col, false)),
                Some((by, false)) if by == col => None,
                _ => Some((col, true)),
            };
        }
        if new_request {
            self.open_new_form(None, None);
        }
        if clear_filters {
            self.filters = Filters::default();
            self.search.clear();
        }
        if show_all {
            self.tab = Tab::All;
        }
    }

    // -----------------------------------------------------------------------
    // Detail view
    // -----------------------------------------------------------------------

    fn detail_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        let Some(id) = self.selected else {
            return;
        };
        let snapshot = Arc::clone(&self.snapshot);
        let Some(request) = snapshot.requests.iter().find(|r| r.request_id == id) else {
            self.selected = None;
            return;
        };
        let can_write = self.can_write();
        let why = self.why_read_only();
        let today = Local::now().date_naive();
        // Opening a request counts as having seen its latest changes.
        if snapshot.unread.contains(&id) && self.last_marked != Some((id, request.changed_at)) {
            self.last_marked = Some((id, request.changed_at));
            self.handle.send(Cmd::MarkSeen(id));
        }
        let dark = ctx.style().visuals.dark_mode;
        let icons = self.icons.clone();

        egui::SidePanel::right("detail")
            .resizable(true)
            .default_width(440.0)
            .min_width(330.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            theme::status_pill(ui, request.status);
                            ui.weak(format!("#{}", request.seq));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(icons.button(ui, theme::Icon::Close, "", "×"))
                                        .on_hover_text(t("Stäng (Esc)", "Close (Esc)"))
                                        .clicked()
                                    {
                                        actions.push(UiAction::CloseDetail);
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(one_line(&request.title)).heading().strong(),
                            )
                            .wrap(),
                        );

                        // Done requests: report, results and summary at the top.
                        if request.status == Status::Done {
                            ui.add_space(8.0);
                            theme::card(ui).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                location_field(
                                    ui,
                                    t("Rapport", "Report"),
                                    request.report.as_ref(),
                                    actions,
                                );
                                location_field(
                                    ui,
                                    t("Resultat", "Results"),
                                    request.results.as_ref(),
                                    actions,
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(t("Sammanfattning", "Summary"))
                                        .size(13.0)
                                        .color(ui.visuals().weak_text_color()),
                                );
                                ui.label(clean_text(&request.summary));
                            });
                        }
                        if matches!(request.status, Status::Failed | Status::Cancelled) {
                            ui.add_space(8.0);
                            theme::card(ui).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(t("Anteckning", "Note"))
                                        .size(13.0)
                                        .color(ui.visuals().weak_text_color()),
                                );
                                if request.note.trim().is_empty() {
                                    ui.weak(t("Ingen anteckning", "No note"));
                                } else {
                                    ui.label(clean_text(&request.note));
                                }
                            });
                        }

                        theme::section(ui, t("Detaljer", "Details"));
                        ui.horizontal_top(|ui| {
                            theme::field_label(ui, t("Avdelning", "Department"));
                            match find_department(&snapshot, request.department) {
                                Some(d) => {
                                    theme::dot(ui, theme::dept_color(d.color, dark));
                                    ui.add(egui::Label::new(one_line(&d.name)).wrap());
                                }
                                None => {
                                    ui.weak("–");
                                }
                            }
                        });
                        if let Some(part) = find_part(&snapshot, request.part) {
                            ui.horizontal_top(|ui| {
                                theme::field_label(ui, t("Del", "Part"));
                                ui.add(egui::Label::new(one_line(&part.name)).wrap());
                                theme::pill(
                                    ui,
                                    part.status.label(),
                                    theme::part_status_color(part.status, dark),
                                );
                            });
                        }
                        text_field(ui, t("Typ", "Type"), &request.sim_type);
                        ui.horizontal_top(|ui| {
                            theme::field_label(ui, t("Prioritet", "Priority"));
                            ui.label(
                                RichText::new(priority_label(request.priority))
                                    .color(theme::priority_color(request.priority, ui)),
                            );
                        });
                        text_field(ui, t("Beställare", "Requester"), &request.requester);
                        text_field(ui, t("Utförare", "Assignee"), &request.assignee);
                        ui.horizontal_top(|ui| {
                            theme::field_label(ui, "Deadline");
                            if request.deadline.is_empty() {
                                ui.weak("–");
                            } else if is_overdue(request, today) {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    format!("{} ({})", request.deadline, t("passerad", "overdue")),
                                );
                            } else {
                                ui.label(&request.deadline);
                            }
                        });
                        text_field(ui, t("Skapad", "Created"), &format_time(request.created_at));
                        if let Some(at) = request.started_at {
                            text_field(ui, t("Påbörjad", "Started"), &format_time(at));
                        }
                        if let Some(at) = request.finished_at {
                            text_field(ui, t("Avslutad", "Finished"), &format_time(at));
                        }
                        location_field(ui, t("Indata", "Input"), request.input.as_ref(), actions);

                        theme::section(ui, t("Beskrivning", "Description"));
                        if request.description.trim().is_empty() {
                            ui.weak(t("Ingen beskrivning", "No description"));
                        } else {
                            ui.label(clean_text(&request.description));
                        }

                        ui.add_space(14.0);
                        ui.horizontal_wrapped(|ui| {
                            let button = |ui: &mut egui::Ui, label: &str| {
                                ui.add_enabled(can_write, egui::Button::new(label))
                                    .on_disabled_hover_text(why)
                                    .clicked()
                            };
                            let primary = |ui: &mut egui::Ui, label: &str| {
                                theme::add_primary(ui, can_write, label)
                                    .on_disabled_hover_text(why)
                                    .clicked()
                            };
                            match request.status {
                                Status::Queued => {
                                    if primary(ui, t("Ta ärendet", "Take request")) {
                                        actions.push(UiAction::Take(id));
                                    }
                                    if button(ui, t("Markera klar…", "Mark as done…")) {
                                        actions.push(UiAction::OpenFinish(id));
                                    }
                                }
                                Status::Running => {
                                    if primary(ui, t("Markera klar…", "Mark as done…")) {
                                        actions.push(UiAction::OpenFinish(id));
                                    }
                                }
                                Status::Failed | Status::Cancelled => {
                                    if primary(ui, t("Köa om", "Requeue")) {
                                        actions.push(UiAction::Requeue(id));
                                    }
                                }
                                Status::Done => {}
                            }
                            if request.status.is_active() {
                                if button(ui, t("Misslyckad…", "Failed…")) {
                                    actions.push(UiAction::OpenNote(id, NoteKind::Fail));
                                }
                                if button(ui, t("Avbryt…", "Cancel…")) {
                                    actions.push(UiAction::OpenNote(id, NoteKind::Cancel));
                                }
                            }
                            if button(ui, t("Duplicera…", "Duplicate…")) {
                                actions.push(UiAction::Duplicate(id));
                            }
                            if button(ui, t("Ta bort…", "Delete…")) {
                                actions.push(UiAction::OpenNote(id, NoteKind::Delete));
                            }
                        });

                        theme::section(
                            ui,
                            &format!(
                                "{} ({})",
                                t("Kommentarer", "Comments"),
                                request.comments.len()
                            ),
                        );
                        ui.add_space(2.0);
                        for comment in &request.comments {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(one_line(&comment.by)).strong());
                                ui.weak(format_time(comment.at_ms));
                            });
                            ui.add(egui::Label::new(clean_text(&comment.text)).wrap());
                            ui.add_space(4.0);
                        }
                        ui.add(
                            egui::TextEdit::multiline(&mut self.comment_draft)
                                .hint_text(t(
                                    "Skriv en fråga eller kommentar…",
                                    "Write a question or a comment…",
                                ))
                                .char_limit(MAX_TEXT)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        );
                        let has_text = !self.comment_draft.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_write && has_text,
                                egui::Button::new(t("Skicka kommentar", "Send comment")),
                            )
                            .on_disabled_hover_text(if has_text {
                                why
                            } else {
                                t("Skriv något först", "Write something first")
                            })
                            .clicked()
                        {
                            actions.push(UiAction::SendComment(id));
                        }

                        theme::section(ui, t("Historik", "History"));
                        ui.add_space(2.0);
                        let empty = Vec::new();
                        for entry in snapshot.history.get(&id).unwrap_or(&empty) {
                            ui.horizontal_wrapped(|ui| {
                                theme::dot(
                                    ui,
                                    if entry.problem.is_some() {
                                        ui.visuals().warn_fg_color
                                    } else {
                                        ui.visuals().weak_text_color()
                                    },
                                );
                                ui.weak(format_time(entry.at_ms));
                                ui.label(RichText::new(one_line(&entry.by)).strong());
                                ui.label(&entry.what);
                            });
                            if let Some(problem) = &entry.problem {
                                ui.indent(("problem", entry.at_ms, &entry.by), |ui| {
                                    ui.colored_label(ui.visuals().warn_fg_color, problem);
                                });
                            }
                        }
                        ui.add_space(10.0);
                    });
            });
    }

    // -----------------------------------------------------------------------
    // Forms
    // -----------------------------------------------------------------------

    fn form_ui(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.form.take() else {
            return;
        };
        let busy = self.pending.is_some();
        let writable = self.snapshot.status == FolderStatus::Ok;
        let why = self.why_read_only();
        let mut close = false;
        let mut send = false;

        egui::Window::new(form.window_title())
            .id(egui::Id::new("form_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(500.0);
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 160.0).max(200.0))
                    .show(ui, |ui| match &mut form {
                        Form::New(f) => new_form_ui(ui, f, &self.snapshot),
                        Form::Finish(f) => finish_form_ui(ui, f),
                        Form::Note(f) => note_form_ui(ui, f),
                    });
                ui.add_space(6.0);
                if let Some(error) = &self.form_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let problem = form.problem();
                // Own line that wraps, so that the window does not grow wider.
                if busy {
                    ui.weak(t("Sparar…", "Saving…"));
                } else if let Some(problem) = &problem {
                    ui.weak(problem);
                } else if !writable {
                    ui.weak(why);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let ready = problem.is_none() && !busy && writable;
                    let hint = problem.clone().unwrap_or_else(|| why.to_string());
                    if theme::add_primary(ui, ready, form.submit_label())
                        .on_disabled_hover_text(hint)
                        .clicked()
                    {
                        send = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(t("Stäng", "Close")))
                        .clicked()
                    {
                        close = true;
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });

        if send {
            let (request_id, action, files) = form.action();
            self.submit(request_id, action, files, Pending::Form);
        }
        if close {
            self.form_error = None;
        } else {
            self.form = Some(form);
        }
    }

    // -----------------------------------------------------------------------
    // Settings and diagnostics
    // -----------------------------------------------------------------------

    fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let snapshot = Arc::clone(&self.snapshot);
        let mut open = true;
        egui::Window::new(t("Inställningar", "Settings"))
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .default_height(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.settings_tab,
                        SettingsTab::Settings,
                        t("Inställningar", "Settings"),
                    );
                    ui.selectable_value(
                        &mut self.settings_tab,
                        SettingsTab::Diagnostics,
                        t("Diagnostik", "Diagnostics"),
                    );
                });
                ui.separator();
                match self.settings_tab {
                    SettingsTab::Settings => {
                        if self.identity_fields(ui, t("Spara", "Save")) {
                            self.save_identity();
                        }
                        if self.identity.folder != self.config.sync_folder {
                            ui.weak(t("När mappen byts läses allt in på nytt från den nya mappen.", "When the folder is changed, everything is read again from the new folder."));
                        }
                        self.update_settings_ui(ui);
                    }
                    SettingsTab::Diagnostics => self.diagnostics_ui(ui, &snapshot),
                }
            });
        if !open {
            self.close_settings();
        }
    }

    fn close_settings(&mut self) {
        self.settings_open = false;
        // Unsaved changes are discarded.
        self.identity.name = self.config.display_name.clone();
        self.identity.folder = self.config.sync_folder.clone();
        self.identity.github = self.config.github.is_some();
        self.identity.team_key.clear();
        self.identity.error = None;
    }

    /// Shows a message in the corner for a few seconds.
    fn toast(&mut self, title: &str, text: String) {
        self.toasts.push(Toast {
            title: title.to_string(),
            text,
            shown: std::time::Instant::now(),
        });
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|toast| toast.shown.elapsed().as_secs_f32() < TOAST_SECONDS);
        if self.toasts.is_empty() {
            return;
        }
        let mut dismissed = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -44.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (index, toast) in self.toasts.iter().enumerate() {
                    let response = egui::Frame::new()
                        .fill(ui.visuals().window_fill)
                        .stroke(ui.visuals().window_stroke)
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(320.0);
                            ui.label(
                                RichText::new(one_line(&toast.title))
                                    .font(theme::semibold_font(14.0))
                                    .color(ui.visuals().strong_text_color()),
                            );
                            if !toast.text.trim().is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(one_line(&toast.text))
                                            .size(12.5)
                                            .color(ui.visuals().weak_text_color()),
                                    )
                                    .wrap(),
                                );
                            }
                        })
                        .response;
                    // A red edge, and a click puts the message away.
                    let edge = egui::Rect::from_min_max(
                        response.rect.left_top(),
                        egui::pos2(response.rect.left() + 3.0, response.rect.bottom()),
                    );
                    ui.painter()
                        .rect_filled(edge, egui::CornerRadius::ZERO, theme::ACCENT);
                    if response.interact(egui::Sense::click()).clicked() {
                        dismissed = Some(index);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(index) = dismissed {
            self.toasts.remove(index);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// Keyboard shortcuts: new request, search, the three views and Esc.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let command = egui::Modifiers::COMMAND;
        let pressed = |key| ctx.input_mut(|i| i.consume_key(command, key));
        if pressed(egui::Key::N) && self.can_write() && self.form.is_none() {
            self.open_new_form(None, None);
        }
        if pressed(egui::Key::F) {
            self.view = View::Queue;
            self.focus_search = true;
        }
        for (key, view) in [
            (egui::Key::Num1, View::Queue),
            (egui::Key::Num2, View::Overview),
            (egui::Key::Num3, View::Stats),
        ] {
            if pressed(key) {
                self.view = view;
            }
        }
        // Esc first leaves a text field (egui does that), then closes the
        // topmost thing that is open.
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if !self.notices.is_empty() {
                self.notices.pop_front();
            } else if self.confirm.is_some() {
                self.confirm = None;
            } else if self.form.is_some() {
                if self.pending.is_none() {
                    self.form = None;
                    self.form_error = None;
                }
            } else if self.settings_open {
                self.close_settings();
            } else if self.view == View::Overview {
                self.board.escape();
            } else if self.view == View::Queue {
                self.selected = None;
            }
        }
    }

    fn diagnostics_ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(440.0)
            .show(ui, |ui| {
                stacked_field(ui, t("Appversion", "App version"), env!("CARGO_PKG_VERSION"));
                stacked_field(ui, t("Antal händelser", "Number of events"), &snapshot.op_count.to_string());
                stacked_field(ui, t("Antal ärenden", "Number of requests"), &snapshot.requests.len().to_string());
                let last_sync = snapshot
                    .last_sync_ms
                    .map(format_time)
                    .unwrap_or_else(|| "–".to_string());
                stacked_field(ui, t("Senaste synk", "Last sync"), &last_sync);
                let folder = snapshot
                    .folder
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_default();
                if let Some(remote) = &snapshot.remote {
                    stacked_field(ui, t("GitHub-repo", "GitHub repository"), &remote.repo);
                    stacked_field(
                        ui,
                        t("Väntar på att skickas", "Waiting to be sent"),
                        &remote.pending.to_string(),
                    );
                    stacked_field(ui, t("Lokal kopia", "Local copy"), &folder);
                } else {
                    stacked_field(ui, t("Delad mapp", "Shared folder"), &folder);
                }
                stacked_field(ui, "Cache", &self.paths.cache_file.display().to_string());
                stacked_field(
                    ui,
                    t("Inställningar", "Settings"),
                    &self.paths.config_file.display().to_string(),
                );
                stacked_field(ui, t("Klient-ID", "Client ID"), &snapshot.client_id.to_string());
                if snapshot.unsupported_seen {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        t("Mappen innehåller händelser från en nyare version. Ny version av appen finns.", "The folder contains events from a newer version. A new version of the app is available."),
                    );
                }

                ui.add_space(8.0);
                let can_rebuild = matches!(
                    snapshot.status,
                    FolderStatus::Ok | FolderStatus::NewerFormat
                );
                if ui
                    .add_enabled(can_rebuild, egui::Button::new(t("Bygg om cachen", "Rebuild cache")))
                    .on_hover_text(t("Tömmer den lokala cachen och läser in alla händelser från mappen igen.", "Clears the local cache and reads all events from the folder again."))
                    .on_disabled_hover_text(t("Cachen kan bara byggas om när mappen går att nå.", "The cache can only be rebuilt when the folder is reachable."))
                    .clicked()
                {
                    self.handle.send(Cmd::Rebuild);
                }

                if !snapshot.orphan_files.is_empty() {
                    ui.add_space(8.0);
                    let size: u64 = snapshot.orphan_files.iter().map(|f| f.size).sum();
                    ui.label(if is_en() {
                        format!(
                            "{} uploaded file(s) ({}) belong only to deleted requests.",
                            snapshot.orphan_files.len(),
                            blobs::size_label(size)
                        )
                    } else {
                        format!(
                            "{} uppladdade fil(er) ({}) hör bara till borttagna ärenden.",
                            snapshot.orphan_files.len(),
                            blobs::size_label(size)
                        )
                    });
                    let busy = self.cleaning.is_some();
                    if ui
                        .add_enabled(
                            !busy && snapshot.status == FolderStatus::Ok,
                            egui::Button::new(t("Rensa filerna…", "Clean up the files…")),
                        )
                        .clicked()
                    {
                        self.confirm = Some(Confirm {
                            title: t("Rensa filer", "Clean up files").to_string(),
                            text: t(
                                "Filerna tas bort för alla i laget och går inte att få tillbaka. Ärendena är redan borttagna.",
                                "The files are removed for everyone on the team and cannot be brought back. The requests are already deleted.",
                            )
                            .to_string(),
                            button: t("Rensa filerna", "Clean up the files").to_string(),
                            what: ConfirmWhat::CleanFiles,
                        });
                    }
                    if busy {
                        ui.spinner();
                    }
                }

                ui.add_space(8.0);
                egui::CollapsingHeader::new(format!(
                    "{} ({})",
                    t("Korrupta eller ogiltiga filer", "Corrupt or invalid files"),
                    snapshot.bad_files.len()
                ))
                .default_open(!snapshot.bad_files.is_empty())
                .show(ui, |ui| {
                    if snapshot.bad_files.is_empty() {
                        ui.weak(t("Inga", "None"));
                    }
                    for file in &snapshot.bad_files {
                        let state = match file.state {
                            SeenState::Retry => {
                                if is_en() {
                                    format!("retrying ({} of {})", file.attempts, sync::MAX_ATTEMPTS)
                                } else {
                                    format!("försöker igen ({} av {})", file.attempts, sync::MAX_ATTEMPTS)
                                }
                            }
                            SeenState::Corrupt => t("korrupt", "corrupt").to_string(),
                            SeenState::Invalid => t("ogiltig", "invalid").to_string(),
                            SeenState::Ok => "ok".to_string(),
                        };
                        ui.label(format!("{} – {state}", one_line(&file.path)));
                        ui.indent(&file.path, |ui| {
                            ui.weak(one_line(&file.error));
                        });
                    }
                });
                egui::CollapsingHeader::new(format!(
                    "{} ({})",
                    t("Ignorerade filer", "Ignored files"),
                    snapshot.ignored_files.len()
                ))
                .show(ui, |ui| {
                    if snapshot.ignored_files.is_empty() {
                        ui.weak(t("Inga", "None"));
                    }
                    for name in snapshot.ignored_files.iter().take(200) {
                        ui.label(one_line(name));
                    }
                    if snapshot.ignored_files.len() > 200 {
                        ui.weak("…");
                    }
                });
                egui::CollapsingHeader::new(format!(
                    "{} ({})",
                    t("Borttagna ärenden", "Deleted requests"),
                    snapshot.deleted.len()
                ))
                .show(ui, |ui| {
                    if snapshot.deleted.is_empty() {
                        ui.weak(t("Inga", "None"));
                    }
                    for item in snapshot.deleted.iter().rev().take(300) {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(format_time(item.at_ms));
                            ui.label(format!(
                                "#{} {}",
                                item.seq,
                                truncate_chars(&one_line(&item.title), 60)
                            ));
                            ui.weak(format!(
                                "({} {})",
                                t("borttaget av", "deleted by"),
                                one_line(&item.by)
                            ));
                        });
                    }
                });
                egui::CollapsingHeader::new(format!(
                    "{} ({})",
                    t("Avvisade händelser", "Rejected events"),
                    snapshot.rejected_ops.len()
                ))
                .show(ui, |ui| {
                    if snapshot.rejected_ops.is_empty() {
                        ui.weak(t("Inga", "None"));
                    }
                    for op in snapshot.rejected_ops.iter().rev().take(300) {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(format_time(op.at_ms));
                            ui.label(RichText::new(one_line(&op.by)).strong());
                            ui.label(&op.what);
                            if !op.title.is_empty() {
                                ui.weak(format!(
                                    "({})",
                                    truncate_chars(&one_line(&op.title), 50)
                                ));
                            }
                        });
                        ui.indent((op.at_ms, &op.by, &op.what), |ui| {
                            ui.weak(&op.problem);
                        });
                    }
                    if snapshot.rejected_ops.len() > 300 {
                        ui.weak("…");
                    }
                });
            });
    }

    /// Asks before something that cannot be undone.
    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else {
            return;
        };
        let can_write = self.can_write();
        let (mut yes, mut no) = (false, false);
        egui::Window::new(one_line(&confirm.title))
            .id(egui::Id::new("confirm_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.add_space(4.0);
                ui.label(&confirm.text);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    yes = theme::add_primary(ui, can_write, &confirm.button).clicked();
                    no = ui.button(t("Avbryt", "Cancel")).clicked();
                });
            });
        if yes {
            if let Some(confirm) = self.confirm.take() {
                match confirm.what {
                    ConfirmWhat::Submit(id, action) => {
                        self.submit(Some(id), *action, Vec::new(), Pending::Other)
                    }
                    ConfirmWhat::CleanFiles => self.clean_files(),
                }
            }
        } else if no {
            self.confirm = None;
        }
    }

    fn notice_ui(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.notices.front() else {
            return;
        };
        let mut dismissed = false;
        egui::Window::new(one_line(&notice.title))
            .id(egui::Id::new("notice_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.add_space(4.0);
                ui.label(&notice.text);
                ui.add_space(10.0);
                if ui.button("OK").clicked() {
                    dismissed = true;
                }
            });
        if dismissed {
            self.notices.pop_front();
        }
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<UiAction>) {
        for action in actions {
            match action {
                UiAction::Select(id) => {
                    if self.selected != Some(id) {
                        self.comment_draft.clear();
                    }
                    self.selected = Some(id);
                }
                UiAction::GoToRequest(id) => {
                    self.view = View::Queue;
                    self.tab = Tab::All;
                    self.filters = Filters::default();
                    self.search.clear();
                    self.comment_draft.clear();
                    self.selected = Some(id);
                }
                UiAction::NewRequest(department, part) => self.open_new_form(department, part),
                UiAction::SendComment(id) => {
                    let text = self.comment_draft.trim().to_string();
                    if !text.is_empty() {
                        self.submit(
                            Some(id),
                            NewAction::Comment { text },
                            Vec::new(),
                            Pending::Comment,
                        );
                    }
                }
                UiAction::Duplicate(id) => {
                    if let Some(request) =
                        self.snapshot.requests.iter().find(|r| r.request_id == id)
                    {
                        // An uploaded file is not copied: it is chosen again if needed.
                        let input = match &request.input {
                            Some(l) if l.kind != LocKind::File => LocInput {
                                kind: l.kind,
                                value: l.value.clone(),
                                ..LocInput::default()
                            },
                            _ => LocInput::default(),
                        };
                        self.form_error = None;
                        self.form = Some(Form::New(NewForm {
                            title: request.title.clone(),
                            description: request.description.clone(),
                            sim_type: request.sim_type.clone(),
                            department: find_department(&self.snapshot, request.department)
                                .map(|d| d.id),
                            part: find_part(&self.snapshot, request.part).map(|p| p.id),
                            priority: request.priority,
                            requester: self.config.display_name.clone(),
                            deadline: String::new(),
                            input,
                        }));
                    }
                }
                UiAction::CloseDetail => self.selected = None,
                UiAction::Take(id) => self.submit(
                    Some(id),
                    NewAction::Take {
                        assignee: self.config.display_name.clone(),
                    },
                    Vec::new(),
                    Pending::Other,
                ),
                UiAction::Requeue(id) => {
                    self.submit(Some(id), NewAction::Requeue, Vec::new(), Pending::Other)
                }
                UiAction::Download(file) => self.start_download(file),
                UiAction::OpenFinish(id) => {
                    if let Some(request) =
                        self.snapshot.requests.iter().find(|r| r.request_id == id)
                    {
                        self.form_error = None;
                        self.form = Some(Form::Finish(FinishForm {
                            request_id: id,
                            title: request.title.clone(),
                            assignee: if request.assignee.trim().is_empty() {
                                self.config.display_name.clone()
                            } else {
                                request.assignee.clone()
                            },
                            report: LocInput::default(),
                            results: LocInput::default(),
                            summary: String::new(),
                        }));
                    }
                }
                UiAction::OpenNote(id, kind) => {
                    if let Some(request) =
                        self.snapshot.requests.iter().find(|r| r.request_id == id)
                    {
                        self.form_error = None;
                        self.form = Some(Form::Note(NoteForm {
                            request_id: id,
                            title: request.title.clone(),
                            kind,
                            note: String::new(),
                        }));
                    }
                }
                UiAction::OpenUrl(url) => {
                    // Only http(s) is opened, never other schemes.
                    if is_http_url(&url) {
                        ctx.open_url(egui::OpenUrl::new_tab(url));
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if !self.config.is_complete() {
            self.setup_ui(ctx);
            self.notice_ui(ctx);
            return;
        }
        self.shortcuts(ctx);
        // A remembered filter on a department that is gone would hide everything.
        if let Some(Some(id)) = self.filters.department {
            if self.snapshot.status == FolderStatus::Ok
                && find_department(&self.snapshot, Some(id)).is_none()
            {
                self.filters.department = None;
            }
        }
        let mut actions = Vec::new();
        self.top_ui(ctx);
        self.status_ui(ctx);
        match self.view {
            View::Queue => {
                self.detail_ui(ctx, &mut actions);
                self.table_ui(ctx, &mut actions);
            }
            View::Overview => self.overview_ui(ctx, &mut actions),
            View::Stats => self.stats_ui(ctx),
        }
        self.form_ui(ctx);
        self.settings_ui(ctx);
        self.confirm_ui(ctx);
        self.notice_ui(ctx);
        self.toast_ui(ctx);
        self.apply(ctx, actions);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let remembered = Remembered {
            view: self.view,
            tab: self.tab,
            filters: self.filters.clone(),
            sort: self.sort,
        };
        eframe::set_value(storage, REMEMBERED_KEY, &remembered);
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        // The window and the view are also saved when the app closes.
        std::time::Duration::from_secs(120)
    }
}

// ---------------------------------------------------------------------------
// Free-standing helper functions
// ---------------------------------------------------------------------------

/// The heading and explanation of an empty view. Buttons go below it.
fn empty_state(ui: &mut egui::Ui, title: &str, text: &str) {
    ui.label(
        RichText::new(title)
            .font(theme::semibold_font(19.0))
            .color(ui.visuals().strong_text_color()),
    );
    ui.weak(text);
    ui.add_space(10.0);
}

/// "⌘N" on a Mac, "Ctrl+N" elsewhere.
fn shortcut_text(ctx: &egui::Context, key: egui::Key) -> String {
    ctx.format_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key))
}

fn apply_theme(ctx: &egui::Context, theme: ThemeChoice) {
    ctx.set_theme(match theme {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    });
}

/// The status bar when the app syncs with GitHub.
fn remote_status_ui(ui: &mut egui::Ui, remote: &RemoteStatus) {
    let (color, text) = match &remote.state {
        RemoteState::Starting => (
            ui.visuals().weak_text_color(),
            t("Ansluter till GitHub…", "Connecting to GitHub…").to_string(),
        ),
        RemoteState::Ok => (
            theme::status_color(Status::Done, ui.visuals().dark_mode),
            format!(
                "{} {}",
                t("Synkad med GitHub", "Synced with GitHub"),
                remote.last_ok_ms.map(format_clock).unwrap_or_default()
            ),
        ),
        RemoteState::Offline => (
            ui.visuals().warn_fg_color,
            t(
                "Ingen kontakt med GitHub, visar sparad data",
                "No contact with GitHub, showing saved data",
            )
            .to_string(),
        ),
        RemoteState::Error(text) => (ui.visuals().error_fg_color, one_line(text)),
    };
    theme::dot(ui, color);
    ui.label(text);
    if remote.pending_files > 0 {
        ui.weak(blobs::pending_label(remote.pending_files));
    }
    if remote.pending > 0 {
        ui.weak(if is_en() {
            format!("· {} change(s) waiting to be sent", remote.pending)
        } else {
            format!("· {} ändring(ar) väntar på att skickas", remote.pending)
        });
    }
}

/// Looks for `simq-teamkey.txt` next to the app (or next to the .app bundle on macOS).
fn find_team_key_file() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(4).find_map(|dir| {
        let text = std::fs::read_to_string(dir.join("simq-teamkey.txt")).ok()?;
        let key = text.trim();
        (!key.is_empty() && key.len() <= 600).then(|| key.to_string())
    })
}

fn matches_search(request: &Request, needle: &str, snapshot: &Snapshot) -> bool {
    needle.is_empty()
        || request.title.to_lowercase().contains(needle)
        || request.requester.to_lowercase().contains(needle)
        || request.assignee.to_lowercase().contains(needle)
        || request.sim_type.to_lowercase().contains(needle)
        || find_department(snapshot, request.department)
            .is_some_and(|d| d.name.to_lowercase().contains(needle))
        || find_part(snapshot, request.part).is_some_and(|p| p.name.to_lowercase().contains(needle))
}

/// The department with the given id, if it still exists.
fn find_department(snapshot: &Snapshot, id: Option<Uuid>) -> Option<&Department> {
    let id = id?;
    snapshot.departments.iter().find(|d| d.id == id)
}

fn find_part(snapshot: &Snapshot, id: Option<Uuid>) -> Option<&Part> {
    let id = id?;
    snapshot.parts.iter().find(|p| p.id == id)
}

fn department_name(snapshot: &Snapshot, id: Option<Uuid>) -> String {
    find_department(snapshot, id)
        .map(|d| truncate_chars(&one_line(&d.name), 40))
        .unwrap_or_else(|| "–".to_string())
}

fn sort_cmp(col: SortCol, a: &Request, b: &Request, snapshot: &Snapshot) -> CmpOrdering {
    let text = |x: &str, y: &str| x.to_lowercase().cmp(&y.to_lowercase());
    // Empty values last, whatever the direction looks like for the rest.
    let empty_last = |x: &str, y: &str| x.is_empty().cmp(&y.is_empty()).then(text(x, y));
    match col {
        SortCol::Seq => a.seq.cmp(&b.seq),
        SortCol::Title => text(&a.title, &b.title),
        SortCol::Department => {
            let name = |r: &Request| {
                find_department(snapshot, r.department)
                    .map(|d| d.name.clone())
                    .unwrap_or_default()
            };
            empty_last(&name(a), &name(b))
        }
        SortCol::Type => empty_last(&a.sim_type, &b.sim_type),
        SortCol::Priority => a.priority.cmp(&b.priority),
        SortCol::Status => (a.status as u8).cmp(&(b.status as u8)),
        SortCol::Requester => text(&a.requester, &b.requester),
        SortCol::Assignee => empty_last(&a.assignee, &b.assignee),
        SortCol::Deadline => empty_last(&a.deadline, &b.deadline),
    }
}

fn is_overdue(request: &Request, today: NaiveDate) -> bool {
    request.status.is_active()
        && NaiveDate::parse_from_str(&request.deadline, "%Y-%m-%d")
            .map(|deadline| deadline < today)
            .unwrap_or(false)
}

fn local_time(ms: u64) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_millis_opt(i64::try_from(ms).ok()?).single()
}

/// Date and time in the user's local time.
fn format_time(ms: u64) -> String {
    local_time(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| t("okänd tid", "unknown time").to_string())
}

fn format_clock(ms: u64) -> String {
    local_time(ms)
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "–".to_string())
}

/// A table cell with one line of text that is clipped if it does not fit.
fn cell(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(one_line(text)).truncate());
}

/// A label and a value on one row, with the labels lined up in a column.
fn text_field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        theme::field_label(ui, label);
        ui.vertical(|ui| {
            if value.trim().is_empty() {
                ui.weak("–");
            } else {
                ui.add(egui::Label::new(one_line(value)).wrap());
            }
        });
    });
}

/// Like `text_field` but with the label above the value, for long labels and paths.
fn stacked_field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(
        RichText::new(label)
            .size(13.0)
            .color(ui.visuals().weak_text_color()),
    );
    if value.trim().is_empty() {
        ui.weak("–");
    } else {
        ui.add(egui::Label::new(one_line(value)).wrap());
    }
    ui.add_space(4.0);
}

/// Shows a location. Only http(s) values become clickable links, everything else is plain text.
fn location_field(
    ui: &mut egui::Ui,
    label: &str,
    location: Option<&Location>,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal_top(|ui| {
        theme::field_label(ui, label);
        ui.vertical(|ui| match location {
            None => {
                ui.weak("–");
            }
            Some(location) if location.kind == LocKind::File => {
                match FileRef::parse(&location.value) {
                    Some(file) => {
                        ui.add(egui::Label::new(one_line(&file.name)).wrap());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    t("Uppladdad fil", "Uploaded file"),
                                    file.size_label()
                                ))
                                .size(12.0)
                                .color(ui.visuals().weak_text_color()),
                            );
                            if ui.small_button(t("Ladda ner", "Download")).clicked() {
                                actions.push(UiAction::Download(file));
                            }
                        });
                    }
                    None => {
                        ui.weak(t("Ogiltig filreferens", "Invalid file reference"));
                    }
                }
            }
            Some(location) => {
                if is_http_url(&location.value) {
                    if ui
                        .link(&location.value)
                        .on_hover_text(t("Öppna i webbläsaren", "Open in the browser"))
                        .clicked()
                    {
                        actions.push(UiAction::OpenUrl(location.value.clone()));
                    }
                } else {
                    ui.add(egui::Label::new(clean_text(&location.value)).wrap());
                }
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(location.kind.label())
                            .size(12.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                    if ui.small_button(t("Kopiera", "Copy")).clicked() {
                        ui.ctx().copy_text(location.value.clone());
                    }
                });
            }
        });
    });
}

/// Quick link in the table row for done requests.
fn quick_link(
    ui: &mut egui::Ui,
    label: &str,
    location: Option<&Location>,
    actions: &mut Vec<UiAction>,
) {
    let Some(location) = location else {
        return;
    };
    if is_http_url(&location.value) {
        if ui
            .link(label)
            .on_hover_text(one_line(&location.value))
            .clicked()
        {
            actions.push(UiAction::OpenUrl(location.value.clone()));
        }
    } else {
        ui.weak(label).on_hover_text(format!(
            "{}: {}",
            location.kind.label(),
            truncate_chars(
                &one_line(
                    &FileRef::parse(&location.value)
                        .map(|file| file.name)
                        .unwrap_or_else(|| location.value.clone())
                ),
                200
            )
        ));
    }
}

fn counter(ui: &mut egui::Ui, text: &str, max: usize) {
    let used = char_len(text);
    if used * 10 >= max * 9 {
        ui.weak(if is_en() {
            format!("{used} of {max} characters")
        } else {
            format!("{used} av {max} tecken")
        });
    }
}

/// Opens the system file picker in its own thread and stores the choice in `input`.
fn pick_file(ctx: &egui::Context, input: &LocInput) {
    let (picked, picking, ctx) = (
        Arc::clone(&input.picked),
        Arc::clone(&input.picking),
        ctx.clone(),
    );
    picking.store(true, Ordering::Relaxed);
    let done = Arc::clone(&picking);
    let spawned = std::thread::Builder::new()
        .name("simq-file-picker".to_string())
        .spawn(move || {
            let choice = rfd::FileDialog::new()
                .set_title(t("Välj fil att ladda upp", "Choose a file to upload"))
                .pick_file();
            if let Some(path) = choice {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let name = blobs::safe_name(
                    &path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
                *picked.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(PickedFile { path, name, size });
            }
            done.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    if spawned.is_err() {
        picking.store(false, Ordering::Relaxed);
    }
}

fn loc_input_ui(ui: &mut egui::Ui, id: &str, input: &mut LocInput) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(input.kind.label())
            .width(120.0)
            .show_ui(ui, |ui| {
                for kind in LocKind::ALL {
                    ui.selectable_value(&mut input.kind, kind, kind.label());
                }
            });
        if input.kind == LocKind::File {
            let picking = input.picking.load(Ordering::Relaxed);
            if ui
                .add_enabled(!picking, egui::Button::new(t("Välj fil…", "Choose file…")))
                .clicked()
            {
                pick_file(ui.ctx(), input);
            }
            match input.picked() {
                Some(picked) => {
                    ui.add(
                        egui::Label::new(format!(
                            "{} ({})",
                            one_line(&picked.name),
                            blobs::size_label(picked.size)
                        ))
                        .truncate(),
                    );
                }
                None => {
                    ui.weak(t("Ingen fil vald", "No file chosen"));
                }
            }
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut input.value)
                    .hint_text(input.kind.placeholder())
                    .char_limit(MAX_TEXT)
                    .desired_width(f32::INFINITY),
            );
        }
    });
    ui.weak(input.kind.help());
}

fn new_form_ui(ui: &mut egui::Ui, f: &mut NewForm, snapshot: &Snapshot) {
    ui.label(RichText::new(t("Titel", "Title")).strong());
    ui.add(
        egui::TextEdit::singleline(&mut f.title)
            .hint_text(t(
                "t.ex. Styvhet bakre upright, lastfall bromsning",
                "e.g. Rear upright stiffness, braking load case",
            ))
            .char_limit(MAX_TITLE)
            .desired_width(f32::INFINITY),
    );
    counter(ui, &f.title, MAX_TITLE);
    ui.add_space(6.0);

    ui.label(RichText::new(t("Beskrivning", "Description")).strong());
    ui.add(
        egui::TextEdit::multiline(&mut f.description)
            .hint_text(t(
                "Vad ska simuleras, vilka lastfall och vad vill du ha svar på?",
                "What should be simulated, which load cases, and what do you want answered?",
            ))
            .char_limit(MAX_TEXT)
            .desired_rows(5)
            .desired_width(f32::INFINITY),
    );
    counter(ui, &f.description, MAX_TEXT);
    ui.add_space(6.0);

    ui.label(RichText::new(t("Avdelning", "Department")).strong());
    if snapshot.departments.is_empty() {
        ui.weak(t(
            "Inga avdelningar än. De läggs till under Översikt.",
            "No departments yet. They are added under Overview.",
        ));
    } else {
        // A department or part that has been removed is dropped from the form.
        if find_department(snapshot, f.department).is_none() {
            f.department = None;
        }
        if find_part(snapshot, f.part).is_none_or(|p| Some(p.department) != f.department) {
            f.part = None;
        }
        let none = t("Ingen", "None");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("new_department")
                .selected_text(match f.department {
                    Some(id) => department_name(snapshot, Some(id)),
                    None => none.to_string(),
                })
                .width(170.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.department, None, none);
                    for d in &snapshot.departments {
                        ui.selectable_value(
                            &mut f.department,
                            Some(d.id),
                            truncate_chars(&one_line(&d.name), 40),
                        );
                    }
                });
            let parts: Vec<&Part> = snapshot
                .parts
                .iter()
                .filter(|p| Some(p.department) == f.department)
                .collect();
            if !parts.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(t("Del", "Part")).strong());
                egui::ComboBox::from_id_salt("new_part")
                    .selected_text(match find_part(snapshot, f.part) {
                        Some(p) => truncate_chars(&one_line(&p.name), 30),
                        None => none.to_string(),
                    })
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.part, None, none);
                        for p in parts {
                            ui.selectable_value(
                                &mut f.part,
                                Some(p.id),
                                truncate_chars(&one_line(&p.name), 40),
                            );
                        }
                    });
            }
        });
    }
    ui.add_space(6.0);

    ui.label(RichText::new(t("Typ", "Type")).strong());
    ui.horizontal(|ui| {
        ui.menu_button(t("Välj…", "Choose…"), |ui| {
            for (sv, en) in SIM_TYPES {
                let sim_type = t(sv, en);
                if ui.button(sim_type).clicked() {
                    f.sim_type = sim_type.to_string();
                    ui.close_menu();
                }
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut f.sim_type)
                .hint_text(t(
                    "välj i listan eller skriv fritt",
                    "choose from the list or type freely",
                ))
                .char_limit(MAX_NAME)
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label(RichText::new(t("Prioritet", "Priority")).strong());
        egui::ComboBox::from_id_salt("priority")
            .selected_text(priority_label(f.priority))
            .width(90.0)
            .show_ui(ui, |ui| {
                for priority in 1..=3u8 {
                    ui.selectable_value(&mut f.priority, priority, priority_label(priority));
                }
            });
        ui.add_space(16.0);
        ui.label(RichText::new("Deadline").strong());
        ui.add(
            egui::TextEdit::singleline(&mut f.deadline)
                .hint_text(t("ÅÅÅÅ-MM-DD (valfri)", "YYYY-MM-DD (optional)"))
                .char_limit(10)
                .desired_width(140.0),
        );
        // A calendar to pick the date from. Typing it still works.
        let today = Local::now().date_naive();
        let mut date = NaiveDate::parse_from_str(f.deadline.trim(), "%Y-%m-%d").unwrap_or(today);
        let before = date;
        ui.add(
            egui_extras::DatePickerButton::new(&mut date)
                .id_salt("deadline_picker")
                .show_icon(true)
                .format(if f.deadline.trim().is_empty() {
                    " "
                } else {
                    "%Y-%m-%d"
                }),
        )
        .on_hover_text(t("Välj datum i kalendern", "Pick the date in the calendar"));
        if date != before {
            f.deadline = date.format("%Y-%m-%d").to_string();
        }
        if !f.deadline.is_empty() && ui.small_button(t("Rensa", "Clear")).clicked() {
            f.deadline.clear();
        }
    });
    ui.add_space(6.0);

    ui.label(RichText::new(t("Beställare", "Requester")).strong());
    ui.add(
        egui::TextEdit::singleline(&mut f.requester)
            .char_limit(MAX_NAME)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    ui.label(RichText::new(t("Indataplats (valfri)", "Input location (optional)")).strong());
    loc_input_ui(ui, "input_kind", &mut f.input);
}

fn finish_form_ui(ui: &mut egui::Ui, f: &mut FinishForm) {
    ui.label(RichText::new(t("Utförare", "Assignee")).strong());
    ui.add(
        egui::TextEdit::singleline(&mut f.assignee)
            .char_limit(MAX_NAME)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);

    ui.label(RichText::new(t("Rapportplats", "Report location")).strong());
    loc_input_ui(ui, "report_kind", &mut f.report);
    ui.add_space(6.0);

    ui.label(RichText::new(t("Resultatplats (valfri)", "Results location (optional)")).strong());
    loc_input_ui(ui, "results_kind", &mut f.results);
    ui.add_space(6.0);

    ui.label(RichText::new(t("Sammanfattning", "Summary")).strong());
    ui.add(
        egui::TextEdit::multiline(&mut f.summary)
            .hint_text(t(
                "Kort: vad visade simuleringen?",
                "Briefly: what did the simulation show?",
            ))
            .char_limit(MAX_TEXT)
            .desired_rows(5)
            .desired_width(f32::INFINITY),
    );
    counter(ui, &f.summary, MAX_TEXT);
}

fn note_form_ui(ui: &mut egui::Ui, f: &mut NoteForm) {
    match f.kind {
        NoteKind::Fail => ui.label(t(
            "Ärendet markeras som misslyckat. Det kan köas om senare.",
            "The request will be marked as failed. It can be requeued later.",
        )),
        NoteKind::Cancel => ui.label(t(
            "Ärendet avbryts. Det kan köas om senare.",
            "The request will be cancelled. It can be requeued later.",
        )),
        NoteKind::Delete => ui.colored_label(
            ui.visuals().warn_fg_color,
            t(
                "Ärendet tas bort för alla i laget. Det går inte att ångra.",
                "The request will be deleted for everyone on the team. This cannot be undone.",
            ),
        ),
    };
    ui.add_space(6.0);
    ui.label(RichText::new(t("Anteckning (valfri)", "Note (optional)")).strong());
    ui.add(
        egui::TextEdit::multiline(&mut f.note)
            .hint_text(match f.kind {
                NoteKind::Fail => t("Vad gick fel?", "What went wrong?"),
                NoteKind::Cancel => t(
                    "Varför avbryts ärendet?",
                    "Why is the request being cancelled?",
                ),
                NoteKind::Delete => t(
                    "Varför tas ärendet bort?",
                    "Why is the request being deleted?",
                ),
            })
            .char_limit(MAX_TEXT)
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    counter(ui, &f.note, MAX_TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PartStatus;
    use crate::store::Store;
    use crate::sync::Submitted;
    use crate::testutil::{create_action, take_action, TempDir};

    fn applied(result: Result<Submitted, String>) -> Uuid {
        match result {
            Ok(Submitted::Applied { request_id, .. }) => request_id,
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn client(folder: &std::path::Path, id: Uuid, name: &str) -> Client {
        Client::new(
            Store::open_in_memory().unwrap(),
            id,
            name.to_string(),
            Some(folder.to_path_buf()),
        )
        .unwrap()
    }

    /// One frame of the whole app, without a window.
    fn frame(ctx: &egui::Context, app: &mut App) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 800.0),
            )),
            ..Default::default()
        };
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame))
    }

    /// Starts the app against a folder that another member has filled.
    /// Returns the app, the department and the request that belongs to it.
    fn app_with_data(shared: &TempDir, local: &TempDir, ctx: &egui::Context) -> (App, Uuid, Uuid) {
        let me = Uuid::new_v4();
        let mut bo = client(shared.path(), Uuid::new_v4(), "Bo");
        let dept = applied(bo.submit(
            None,
            &NewAction::DeptSet {
                regions: vec!["front_wing".to_string(), "rear_wing".to_string()],
                name: "Aero".to_string(),
                color: 0,
                zones: Vec::new(),
            },
        ));
        let part = applied(bo.submit(
            None,
            &NewAction::PartSet {
                department: dept,
                name: "Bakvinge".to_string(),
                status: PartStatus::Simulation,
                note: String::new(),
            },
        ));
        let mut create = create_action("Downforce bakvinge");
        if let NewAction::Create {
            department,
            part: for_part,
            sim_type,
            ..
        } = &mut create
        {
            *department = Some(dept);
            *for_part = Some(part);
            *sim_type = "CFD".to_string();
        }
        let request = applied(bo.submit(None, &create));
        applied(bo.submit(None, &create_action("Upright")));
        applied(bo.submit(
            Some(request),
            &NewAction::Comment {
                text: "Vilken hastighet?".to_string(),
            },
        ));
        applied(bo.submit(Some(request), &take_action("Bo")));

        let config = Config {
            sync_folder: Some(shared.path().to_path_buf()),
            github: None,
            display_name: "Anna".to_string(),
            language: Lang::Sv,
            theme: ThemeChoice::System,
            client_id: me,
        };
        let paths = Paths {
            config_file: local.path().join("config.json"),
            cache_file: local.path().join("cache.sqlite"),
            mirror_root: local.path().join("mirror"),
            legacy_config_file: None,
        };
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, client(shared.path(), me, "Anna"), config, paths).unwrap();
        // Wait for the background thread's first sync.
        for _ in 0..200 {
            frame(ctx, &mut app);
            if app.snapshot.status == FolderStatus::Ok && app.snapshot.requests.len() == 2 {
                return (app, dept, request);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the first sync never finished: {:?}", app.snapshot.status);
    }

    #[test]
    fn every_view_renders_and_filters_narrow_the_queue() {
        let (shared, local) = (TempDir::new(), TempDir::new());
        let ctx = egui::Context::default();
        let (mut app, dept, request) = app_with_data(&shared, &local, &ctx);
        let snapshot = Arc::clone(&app.snapshot);
        assert_eq!(snapshot.departments.len(), 1);
        assert_eq!(snapshot.parts.len(), 1);

        // The queue with a request open, its comments and the new request form.
        app.apply(&ctx, vec![UiAction::Select(request)]);
        frame(&ctx, &mut app);
        app.apply(&ctx, vec![UiAction::NewRequest(Some(dept), None)]);
        frame(&ctx, &mut app);
        assert!(matches!(&app.form, Some(Form::New(f)) if f.department == Some(dept)));
        app.form = None;
        app.apply(&ctx, vec![UiAction::Duplicate(request)]);
        frame(&ctx, &mut app);
        match &app.form {
            Some(Form::New(f)) => {
                assert_eq!(f.title, "Downforce bakvinge");
                assert_eq!((f.department, f.requester.as_str()), (Some(dept), "Anna"));
                assert!(f.part.is_some());
            }
            _ => panic!("the duplicate should open the new request form"),
        }
        app.form = None;

        // Filters, search and sorting.
        assert_eq!(app.filtered(&snapshot, None).len(), 2);
        app.filters.department = Some(Some(dept));
        assert_eq!(app.filtered(&snapshot, None).len(), 1);
        assert_eq!(app.filtered(&snapshot, Some(Tab::Queued)).len(), 0);
        app.filters.department = Some(None);
        assert_eq!(app.filtered(&snapshot, None)[0].title, "Upright");
        app.filters = Filters {
            sim_type: Some("CFD".to_string()),
            person: Some("Bo".to_string()),
            priority: Some(2),
            ..Filters::default()
        };
        assert_eq!(app.filtered(&snapshot, None).len(), 1);
        app.filters = Filters {
            mine: true,
            ..Filters::default()
        };
        // Both requests name Anna as the requester.
        assert_eq!(app.filtered(&snapshot, None).len(), 2);
        app.config.display_name = "Cia".to_string();
        assert!(app.filtered(&snapshot, None).is_empty());
        app.config.display_name = "Anna".to_string();
        app.filters = Filters::default();
        app.search = "aero".to_string();
        assert_eq!(
            app.filtered(&snapshot, None).len(),
            1,
            "search finds the department"
        );
        app.search = "bakvinge".to_string();
        assert_eq!(app.filtered(&snapshot, None).len(), 1);
        app.search.clear();
        frame(&ctx, &mut app);

        let mut rows = app.filtered(&snapshot, None);
        app.sort = Some((SortCol::Department, true));
        app.sort_rows(&mut rows, &snapshot);
        assert_eq!(
            rows[0].title, "Downforce bakvinge",
            "no department sorts last"
        );
        app.sort = Some((SortCol::Title, false));
        app.sort_rows(&mut rows, &snapshot);
        assert_eq!(rows[0].title, "Upright");
        app.sort = None;

        // The other views.
        for view in [View::Overview, View::Stats, View::Queue] {
            app.view = view;
            frame(&ctx, &mut app);
            frame(&ctx, &mut app);
        }
        // From the overview a request can be shown in the queue.
        app.view = View::Overview;
        app.filters.mine = true;
        app.apply(&ctx, vec![UiAction::GoToRequest(request)]);
        assert_eq!((app.view, app.selected), (View::Queue, Some(request)));
        assert!(!app.filters.any());
        app.settings_open = true;
        app.settings_tab = SettingsTab::Diagnostics;
        frame(&ctx, &mut app);
    }

    #[test]
    fn a_comment_is_saved_and_the_field_is_emptied() {
        let (shared, local) = (TempDir::new(), TempDir::new());
        let ctx = egui::Context::default();
        let (mut app, _, request) = app_with_data(&shared, &local, &ctx);
        app.apply(&ctx, vec![UiAction::Select(request)]);
        app.comment_draft = "  60 km/h  ".to_string();
        app.apply(&ctx, vec![UiAction::SendComment(request)]);
        for _ in 0..200 {
            frame(&ctx, &mut app);
            if app.pending.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.comment_draft.is_empty());
        let snapshot = app.handle.snapshot();
        let saved = snapshot
            .requests
            .iter()
            .find(|r| r.request_id == request)
            .unwrap();
        assert_eq!(saved.comments.len(), 2);
        assert_eq!(saved.comments[1].text, "60 km/h");
        assert_eq!(saved.comments[1].by, "Anna");
    }
}
