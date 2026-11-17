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
mod forms;
mod overview;
mod queue;
mod settings;
mod stats;

use forms::*;
use queue::*;
use settings::*;

use crate::blobs::{self, FileRef};
use crate::config::{Config, Paths, ThemeChoice};
use crate::github::{GithubRemote, RemoteState, RemoteStatus, RemoteStore};
use crate::i18n::{self, is_en, t, Lang};
use crate::model::{
    char_len, clean_text, is_http_url, list_order, one_line, priority_label, truncate_chars,
    valid_deadline, Department, LocKind, Location, Part, Request, Status, MAX_ATTACHMENTS,
    MAX_NAME, MAX_TEXT, MAX_TITLE,
};
use crate::ops::NewAction;
use crate::store::SeenState;
use crate::sync::{
    self, Client, Cmd, Event, FileSlot, FolderStatus, Handle, Notice, Snapshot, Source,
};
use crate::theme;
use crate::update;

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

/// What the user clicked during a frame. Collected and carried out afterwards.
enum UiAction {
    Select(Uuid),
    CloseDetail,
    Take(Uuid),
    Requeue(Uuid),
    OpenFinish(Uuid),
    OpenNote(Uuid, NoteKind),
    /// Opens the form that adds an extra file to a request.
    OpenAttach(Uuid),
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
        // Look for a newer version once at start-up (not from the tests: it
        // uses the network). A .deb package is updated by installing a new one.
        if !cfg!(test) && update::install_kind() == update::Install::Portable {
            app.check_for_update(false);
        }
        Ok(app)
    }

    fn team_token(&self) -> Option<String> {
        self.config.github.as_ref().map(|g| g.token.clone())
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
        if focused != self.was_focused {
            self.handle.send(Cmd::Background(!focused));
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

    // -----------------------------------------------------------------------
    // First run
    // -----------------------------------------------------------------------

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
                            && theme::named(
                                ui.add(self.icons.button(ui, theme::Icon::Close, "", "×").small()),
                                t("Rensa sökningen", "Clear the search"),
                            )
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
                        let name = if dark {
                            t("Byt till ljust läge", "Switch to light mode")
                        } else {
                            t("Byt till mörkt läge", "Switch to dark mode")
                        };
                        if theme::named(ui.add(self.icons.button(ui, icon, "", fallback)), name)
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
                let now = crate::sync::now_ms();
                let searching = !self.search.trim().is_empty();
                ui.horizontal_wrapped(|ui| {
                    for tab in Tab::ALL {
                        let count = base
                            .iter()
                            .filter(|r| tab.matches(r, now, searching))
                            .count();
                        ui.selectable_value(
                            &mut self.tab,
                            tab,
                            format!("{}  {count}", tab.label()),
                        );
                    }
                });
                ui.add_space(2.0);
                self.filter_ui(ui, &snapshot);
                self.deadline_reminder_ui(ui, &snapshot);
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

    // -----------------------------------------------------------------------
    // Detail view
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Forms
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Settings and diagnostics
    // -----------------------------------------------------------------------

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

    /// Shows a message in a window that stays until the user closes it.
    pub fn show_notice(&mut self, title: String, text: String) {
        self.notices.push_back(Notice { title, text });
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
                    let archived = self
                        .snapshot
                        .requests
                        .iter()
                        .any(|r| r.request_id == id && is_archived(r, crate::sync::now_ms()));
                    self.tab = if archived { Tab::Archive } else { Tab::All };
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
                UiAction::OpenAttach(id) => {
                    if let Some(request) =
                        self.snapshot.requests.iter().find(|r| r.request_id == id)
                    {
                        self.form_error = None;
                        self.form = Some(Form::Attach(AttachForm {
                            request_id: id,
                            title: request.title.clone(),
                            location: LocInput {
                                kind: LocKind::File,
                                ..LocInput::default()
                            },
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
    if let Some(warning) = remote
        .token_expires_ms
        .and_then(|at| token_warning(at, crate::sync::now_ms()))
    {
        ui.separator();
        ui.colored_label(ui.visuals().warn_fg_color, warning)
            .on_hover_text(t(
                "Den som skapade teamnyckeln på GitHub kan förlänga den eller skapa en ny. En ny nyckel klistras in under Inställningar.",
                "Whoever created the team key on GitHub can extend it or create a new one. A new key is pasted under Settings.",
            ));
    }
}

/// Days before the team key expires that the warning starts showing.
const TOKEN_WARNING_DAYS: u64 = 14;

/// A warning when the team key expires within two weeks.
fn token_warning(expires_ms: u64, now_ms: u64) -> Option<String> {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    if expires_ms > now_ms + TOKEN_WARNING_DAYS * DAY {
        return None;
    }
    if expires_ms <= now_ms {
        return Some(t("Teamnyckeln har gått ut", "The team key has expired").to_string());
    }
    let days = (expires_ms - now_ms).div_ceil(DAY);
    let date = local_time(expires_ms)
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    Some(match (is_en(), days) {
        (true, 1) => format!("The team key expires within a day ({date})"),
        (true, _) => format!("The team key expires in {days} days ({date})"),
        (false, 1) => format!("Teamnyckeln går ut inom ett dygn ({date})"),
        (false, _) => format!("Teamnyckeln går ut om {days} dagar ({date})"),
    })
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
            last_seen_version: None,
            token_in_keychain: false,
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

        // The form for an extra file starts on an uploaded file and needs one.
        app.apply(&ctx, vec![UiAction::OpenAttach(request)]);
        frame(&ctx, &mut app);
        match &mut app.form {
            Some(form @ Form::Attach(_)) => {
                assert!(form.problem().is_some(), "no file chosen yet");
                if let Form::Attach(f) = form {
                    f.location.kind = LocKind::Vault;
                    f.location.value = "CAD/upright.iam".to_string();
                }
                assert_eq!(form.problem(), None);
                let (id, action, files) = form.action();
                assert_eq!(id, Some(request));
                assert!(files.is_empty());
                assert!(matches!(
                    action,
                    NewAction::Attach { location: Some(l) } if l.kind == LocKind::Vault
                ));
            }
            _ => panic!("the attach form should open"),
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
    fn old_finished_requests_move_to_the_archive() {
        const DAY: u64 = 24 * 60 * 60 * 1000;
        let now = 1_800_000_000_000;
        let shared = TempDir::new();
        let local = TempDir::new();
        let ctx = egui::Context::default();
        let (app, _, _) = app_with_data(&shared, &local, &ctx);
        let mut r = app.snapshot.requests[0].clone();
        r.status = Status::Done;
        r.finished_at = Some(now - 31 * DAY);
        assert!(is_archived(&r, now));
        assert!(Tab::Archive.matches(&r, now, false));
        assert!(!Tab::All.matches(&r, now, false));
        assert!(!Tab::Done.matches(&r, now, false));
        assert!(Tab::All.matches(&r, now, true), "found when searching");
        r.finished_at = Some(now - 29 * DAY);
        assert!(!is_archived(&r, now));
        assert!(Tab::Done.matches(&r, now, false));
        assert!(!Tab::Archive.matches(&r, now, false));
        // An active request is never archived.
        r.status = Status::Queued;
        r.finished_at = Some(now - 90 * DAY);
        assert!(!is_archived(&r, now));
    }

    #[test]
    fn deadlines_close_by_are_pointed_out() {
        let shared = TempDir::new();
        let local = TempDir::new();
        let ctx = egui::Context::default();
        let (app, _, _) = app_with_data(&shared, &local, &ctx);
        let today = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        let mut r = app.snapshot.requests[0].clone();
        r.status = Status::Queued;
        for (deadline, due) in [
            ("", false),
            ("2026-11-10", true),
            ("2026-11-17", true),
            ("2026-11-20", true),
            ("2026-11-21", false),
        ] {
            r.deadline = deadline.to_string();
            assert_eq!(is_due_soon(&r, today), due, "{deadline}");
        }
        r.deadline = "2026-11-10".to_string();
        r.status = Status::Done;
        assert!(!is_due_soon(&r, today));
    }

    #[test]
    fn the_team_key_warning_starts_two_weeks_before() {
        const DAY: u64 = 24 * 60 * 60 * 1000;
        let now = 1_800_000_000_000;
        crate::i18n::with_lang(crate::i18n::Lang::En, || {
            assert_eq!(token_warning(now + 15 * DAY, now), None);
            assert!(token_warning(now + 14 * DAY, now)
                .unwrap()
                .contains("in 14 days"));
            assert!(token_warning(now + DAY / 2, now)
                .unwrap()
                .contains("within a day"));
            assert_eq!(
                token_warning(now, now).as_deref(),
                Some("The team key has expired")
            );
        });
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
