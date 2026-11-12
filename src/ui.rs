//! The egui user interface. Never does file I/O: all of that happens in the background
//! thread (`sync::spawn`) and the interface only reads finished snapshots.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use chrono::{Local, NaiveDate, TimeZone};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use uuid::Uuid;

use crate::config::{Config, Paths};
use crate::github::{RemoteState, RemoteStatus};
use crate::i18n::{self, is_en, t, Lang};
use crate::model::{
    char_len, clean_text, is_http_url, list_order, one_line, priority_label, truncate_chars,
    valid_deadline, LocKind, Location, Request, Status, MAX_NAME, MAX_TEXT, MAX_TITLE,
};
use crate::ops::NewAction;
use crate::store::SeenState;
use crate::sync::{self, Client, Cmd, Event, FolderStatus, Handle, Notice, Snapshot, Source};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
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
}

impl LocInput {
    fn location(&self) -> Option<Location> {
        let value = self.value.trim();
        if value.is_empty() {
            None
        } else {
            Some(Location {
                kind: self.kind,
                value: value.to_string(),
            })
        }
    }

    fn problem(&self, what: &str, required: bool) -> Option<String> {
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

    fn action(&self) -> (Option<Uuid>, NewAction) {
        match self {
            Form::New(f) => (
                None,
                NewAction::Create {
                    title: f.title.trim().to_string(),
                    description: f.description.trim().to_string(),
                    sim_type: f.sim_type.trim().to_string(),
                    priority: f.priority,
                    requester: f.requester.trim().to_string(),
                    deadline: f.deadline.trim().to_string(),
                    input: f.input.location(),
                },
            ),
            Form::Finish(f) => (
                Some(f.request_id),
                NewAction::Finish {
                    assignee: f.assignee.trim().to_string(),
                    report: f.report.location(),
                    results: f.results.location(),
                    summary: f.summary.trim().to_string(),
                },
            ),
            Form::Note(f) => {
                let note = f.note.trim().to_string();
                let action = match f.kind {
                    NoteKind::Fail => NewAction::Fail { note },
                    NoteKind::Cancel => NewAction::Cancel { note },
                    NoteKind::Delete => NewAction::Delete { note },
                };
                (Some(f.request_id), action)
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
    tab: Tab,
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
        let mut app = App {
            handle,
            config,
            paths,
            ctx,
            snapshot,
            tab: Tab::Active,
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
        };
        // Look for a newer version once at start-up.
        app.check_for_update(false);
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

    fn submit(&mut self, request_id: Option<Uuid>, action: NewAction) {
        let token = self.token();
        self.pending = Some(token);
        self.form_error = None;
        self.handle.send(Cmd::Submit {
            token,
            request_id,
            action,
        });
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
                    match result {
                        Ok(()) => {
                            self.form = None;
                            self.form_error = None;
                        }
                        Err(text) if self.form.is_some() => self.form_error = Some(text),
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
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    theme::logo(ui, logo.as_ref(), 26.0);
                    ui.label(RichText::new("SimQ").size(20.0).strong());
                    ui.add_space(14.0);
                    if theme::add_primary(ui, can_write, t("+ Ny förfrågan", "+ New request"))
                        .on_disabled_hover_text(self.why_read_only())
                        .clicked()
                    {
                        self.form_error = None;
                        self.form = Some(Form::New(NewForm {
                            title: String::new(),
                            description: String::new(),
                            sim_type: String::new(),
                            priority: 2,
                            requester: self.config.display_name.clone(),
                            deadline: String::new(),
                            input: LocInput::default(),
                        }));
                    }
                    ui.add_space(12.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text(t(
                                "Sök titel, beställare eller typ",
                                "Search title, requester or type",
                            ))
                            .desired_width(260.0),
                    );
                    if !self.search.is_empty() && ui.small_button(t("Rensa", "Clear")).clicked() {
                        self.search.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t("Inställningar", "Settings")).clicked() {
                            self.settings_open = true;
                            self.identity.saved = false;
                        }
                        if ui.button(t("Synka nu", "Sync now")).clicked() {
                            self.handle.send(Cmd::SyncNow);
                        }
                        self.update_button(ui);
                    });
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for tab in Tab::ALL {
                        let count = snapshot
                            .requests
                            .iter()
                            .filter(|r| tab.matches(r.status))
                            .count();
                        ui.selectable_value(
                            &mut self.tab,
                            tab,
                            format!("{}  {count}", tab.label()),
                        );
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
        let needle = self.search.trim().to_lowercase();
        let tab = self.tab;
        let selected = self.selected;
        let mut rows: Vec<&Request> = snapshot
            .requests
            .iter()
            .filter(|r| tab.matches(r.status) && matches_search(r, &needle))
            .collect();
        rows.sort_by(|a, b| list_order(a, b));
        let today = Local::now().date_naive();

        egui::CentralPanel::default().show(ctx, |ui| {
            if rows.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    if snapshot.requests.is_empty() {
                        ui.weak(t(
                            "Inga förfrågningar än. Klicka på \"+ Ny förfrågan\" för att lägga upp den första.",
                            "No requests yet. Click \"+ New request\" to add the first one.",
                        ));
                    } else if needle.is_empty() {
                        ui.weak(t("Inga ärenden under den här fliken.", "No requests under this tab."));
                    } else {
                        ui.weak(t("Inga ärenden matchar sökningen.", "No requests match the search."));
                    }
                });
                return;
            }
            // A click should select the row, not select text.
            ui.style_mut().interaction.selectable_labels = false;
            let overdue_color = ui.visuals().error_fg_color;
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(40.0).at_least(30.0).clip(true))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::initial(130.0).at_least(50.0).clip(true))
                .column(Column::initial(70.0).at_least(50.0).clip(true))
                .column(Column::initial(110.0).at_least(70.0).clip(true))
                .column(Column::initial(120.0).at_least(60.0).clip(true))
                .column(Column::initial(120.0).at_least(60.0).clip(true))
                .column(Column::initial(90.0).at_least(70.0).clip(true))
                .column(Column::initial(130.0).at_least(60.0).clip(true))
                .header(26.0, |mut header| {
                    for title in [
                        "#",
                        t("Titel", "Title"),
                        t("Typ", "Type"),
                        t("Prioritet", "Priority"),
                        "Status",
                        t("Beställare", "Requester"),
                        t("Utförare", "Assignee"),
                        "Deadline",
                        t("Rapport / resultat", "Report / results"),
                    ] {
                        header.col(|ui| {
                            ui.label(
                                RichText::new(title)
                                    .size(12.0)
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let request = rows[row.index()];
                        row.set_selected(selected == Some(request.request_id));
                        row.col(|ui| {
                            ui.weak(request.seq.to_string());
                        });
                        row.col(|ui| {
                            cell(ui, &request.title);
                        });
                        row.col(|ui| {
                            cell(ui, &request.sim_type);
                        });
                        row.col(|ui| {
                            let text = RichText::new(priority_label(request.priority))
                                .color(theme::priority_color(request.priority, ui));
                            ui.label(if request.priority == 1 {
                                text.strong()
                            } else {
                                text
                            });
                        });
                        row.col(|ui| {
                            theme::status_pill(ui, request.status);
                        });
                        row.col(|ui| {
                            cell(ui, &request.requester);
                        });
                        row.col(|ui| {
                            cell(ui, &request.assignee);
                        });
                        row.col(|ui| {
                            if is_overdue(request, today) {
                                ui.colored_label(overdue_color, &request.deadline)
                                    .on_hover_text(t("Deadline har passerat", "The deadline has passed"));
                            } else {
                                ui.label(&request.deadline);
                            }
                        });
                        row.col(|ui| {
                            if request.status == Status::Done {
                                quick_link(ui, t("Rapport", "Report"), request.report.as_ref(), actions);
                                quick_link(ui, t("Resultat", "Results"), request.results.as_ref(), actions);
                            }
                        });
                        if row.response().clicked() {
                            actions.push(UiAction::Select(request.request_id));
                        }
                    });
                });
        });
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
                                    if ui.button(t("Stäng", "Close")).clicked() {
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
                            if button(ui, t("Ta bort…", "Delete…")) {
                                actions.push(UiAction::OpenNote(id, NoteKind::Delete));
                            }
                        });

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
                        Form::New(f) => new_form_ui(ui, f),
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
            let (request_id, action) = form.action();
            self.submit(request_id, action);
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
            self.settings_open = false;
            // Unsaved changes are discarded.
            self.identity.name = self.config.display_name.clone();
            self.identity.folder = self.config.sync_folder.clone();
            self.identity.github = self.config.github.is_some();
            self.identity.team_key.clear();
            self.identity.error = None;
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
                UiAction::Select(id) => self.selected = Some(id),
                UiAction::CloseDetail => self.selected = None,
                UiAction::Take(id) => self.submit(
                    Some(id),
                    NewAction::Take {
                        assignee: self.config.display_name.clone(),
                    },
                ),
                UiAction::Requeue(id) => self.submit(Some(id), NewAction::Requeue),
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
        let mut actions = Vec::new();
        self.top_ui(ctx);
        self.status_ui(ctx);
        self.detail_ui(ctx, &mut actions);
        self.table_ui(ctx, &mut actions);
        self.form_ui(ctx);
        self.settings_ui(ctx);
        self.notice_ui(ctx);
        self.apply(ctx, actions);
    }
}

// ---------------------------------------------------------------------------
// Free-standing helper functions
// ---------------------------------------------------------------------------

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

fn matches_search(request: &Request, needle: &str) -> bool {
    needle.is_empty()
        || request.title.to_lowercase().contains(needle)
        || request.requester.to_lowercase().contains(needle)
        || request.sim_type.to_lowercase().contains(needle)
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
            truncate_chars(&one_line(&location.value), 200)
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
        ui.add(
            egui::TextEdit::singleline(&mut input.value)
                .hint_text(input.kind.placeholder())
                .char_limit(MAX_TEXT)
                .desired_width(f32::INFINITY),
        );
    });
    ui.weak(input.kind.help());
}

fn new_form_ui(ui: &mut egui::Ui, f: &mut NewForm) {
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
