//! The first-run dialog, Settings with Diagnostics, and the in-app update.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTab {
    Settings,
    Diagnostics,
}

/// Name and sync source, used both at first run and under Settings.
pub(super) struct IdentityForm {
    pub(super) name: String,
    pub(super) folder: Option<PathBuf>,
    /// True = sync via GitHub with a team key, false = shared folder.
    pub(super) github: bool,
    pub(super) team_key: String,
    pub(super) error: Option<String>,
    pub(super) saved: bool,
}

/// Looks for `simq-teamkey.txt` next to the app (or next to the .app bundle on macOS).
pub(super) fn find_team_key_file() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(4).find_map(|dir| {
        let text = std::fs::read_to_string(dir.join("simq-teamkey.txt")).ok()?;
        let key = text.trim();
        (!key.is_empty() && key.len() <= 600).then(|| key.to_string())
    })
}

/// Like `text_field` but with the label above the value, for long labels and paths.
pub(super) fn stacked_field(ui: &mut egui::Ui, label: &str, value: &str) {
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

impl App {
    /// Asks GitHub for a newer version in a background thread.
    pub(super) fn check_for_update(&mut self, manual: bool) {
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
    pub(super) fn install_update(&mut self, available: update::Available) {
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

    pub(super) fn restart_into_update(&mut self) {
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
    pub(super) fn update_button(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn update_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.separator();
        ui.label(RichText::new("Version").strong());
        if update::install_kind() == update::Install::LinuxPackage {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("SimQ {}", update::current_version()));
                ui.weak(t(
                    "Installerad som paket. Uppdatera genom att installera den nya .deb-filen från Releases-sidan.",
                    "Installed as a package. Update by installing the new .deb file from the Releases page.",
                ));
            });
            return;
        }
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

    /// Opens the system folder picker in its own thread so the interface does not freeze.
    pub(super) fn pick_folder(&mut self) {
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

    pub(super) fn save_identity(&mut self) {
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
    pub(super) fn identity_fields(&mut self, ui: &mut egui::Ui, button: &str) -> bool {
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

    pub(super) fn setup_ui(&mut self, ctx: &egui::Context) {
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

    pub(super) fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let snapshot = Arc::clone(&self.snapshot);
        let mut open = true;
        // Never taller than the window, so that the title bar with the close
        // button stays on screen. The content scrolls instead.
        let room = (ctx.screen_rect().height() - 60.0).max(160.0);
        egui::Window::new(t("Inställningar", "Settings"))
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .default_height(480.0_f32.min(room))
            .max_height(room)
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
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.settings_tab {
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
                    });
            });
        if !open {
            self.close_settings();
        }
    }

    pub(super) fn close_settings(&mut self) {
        self.settings_open = false;
        // Unsaved changes are discarded.
        self.identity.name = self.config.display_name.clone();
        self.identity.folder = self.config.sync_folder.clone();
        self.identity.github = self.config.github.is_some();
        self.identity.team_key.clear();
        self.identity.error = None;
    }

    /// Shown inside the scrolling settings window.
    pub(super) fn diagnostics_ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.vertical(|ui| {
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
}
