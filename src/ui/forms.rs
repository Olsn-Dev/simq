//! The forms: new request, mark as done, failed/cancelled/deleted, and extra files.

use super::*;

/// Suggested simulation types (Swedish, English).
pub(super) const SIM_TYPES: [(&str, &str); 8] = [
    ("FEA / hållfasthet", "FEA / structural"),
    ("CFD / aerodynamik", "CFD / aerodynamics"),
    ("Termisk", "Thermal"),
    ("Modal / vibration", "Modal / vibration"),
    ("Topologioptimering", "Topology optimisation"),
    ("Fordonsdynamik", "Vehicle dynamics"),
    ("Elektromagnetisk", "Electromagnetic"),
    ("Annat", "Other"),
];

/// Input of a location: kind + value.
#[derive(Default)]
pub(super) struct LocInput {
    pub(super) kind: LocKind,
    pub(super) value: String,
    /// The file chosen for upload, filled in by the file picker thread.
    pub(super) picked: Arc<Mutex<Option<PickedFile>>>,
    pub(super) picking: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct PickedFile {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) size: u64,
}

impl LocInput {
    pub(super) fn picked(&self) -> Option<PickedFile> {
        self.picked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The file to upload, if this location is an uploaded file.
    pub(super) fn file(&self) -> Option<PathBuf> {
        if self.kind == LocKind::File {
            self.picked().map(|picked| picked.path)
        } else {
            None
        }
    }

    pub(super) fn location(&self) -> Option<Location> {
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

    pub(super) fn problem(&self, what: &str, required: bool) -> Option<String> {
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

pub(super) struct NewForm {
    pub(super) title: String,
    pub(super) description: String,
    pub(super) sim_type: String,
    pub(super) department: Option<Uuid>,
    pub(super) part: Option<Uuid>,
    pub(super) priority: u8,
    pub(super) requester: String,
    pub(super) deadline: String,
    pub(super) input: LocInput,
}

pub(super) struct FinishForm {
    pub(super) request_id: Uuid,
    pub(super) title: String,
    pub(super) assignee: String,
    pub(super) report: LocInput,
    pub(super) results: LocInput,
    pub(super) summary: String,
}

pub(super) struct AttachForm {
    pub(super) request_id: Uuid,
    pub(super) title: String,
    pub(super) location: LocInput,
}

pub(super) struct NoteForm {
    pub(super) request_id: Uuid,
    pub(super) title: String,
    pub(super) kind: NoteKind,
    pub(super) note: String,
}

/// What a note form does when submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoteKind {
    Fail,
    Cancel,
    Delete,
}

pub(super) enum Form {
    New(NewForm),
    Finish(FinishForm),
    Note(NoteForm),
    Attach(AttachForm),
}

impl Form {
    pub(super) fn window_title(&self) -> String {
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
            Form::Attach(f) => format!(
                "{}: {}",
                t("Lägg till fil", "Add file"),
                truncate_chars(&one_line(&f.title), 40)
            ),
        }
    }

    pub(super) fn submit_label(&self) -> &'static str {
        match self {
            Form::New(_) => t("Lägg i kön", "Add to queue"),
            Form::Finish(_) => t("Markera klar", "Mark as done"),
            Form::Note(f) => match f.kind {
                NoteKind::Fail => t("Markera misslyckad", "Mark as failed"),
                NoteKind::Cancel => t("Avbryt ärendet", "Cancel the request"),
                NoteKind::Delete => t("Ta bort ärendet", "Delete the request"),
            },
            Form::Attach(_) => t("Lägg till", "Add"),
        }
    }

    /// The first problem that prevents the form from being submitted.
    pub(super) fn problem(&self) -> Option<String> {
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
            Form::Attach(f) => f.location.problem(t("Fil", "File"), true),
        }
    }

    pub(super) fn action(&self) -> (Option<Uuid>, NewAction, Vec<(FileSlot, PathBuf)>) {
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
            Form::Attach(f) => (
                Some(f.request_id),
                NewAction::Attach {
                    location: f.location.location(),
                },
                f.location
                    .file()
                    .map(|path| (FileSlot::Attachment, path))
                    .into_iter()
                    .collect(),
            ),
        }
    }
}

pub(super) fn counter(ui: &mut egui::Ui, text: &str, max: usize) {
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
pub(super) fn pick_file(ctx: &egui::Context, input: &LocInput) {
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

pub(super) fn loc_input_ui(ui: &mut egui::Ui, id: &str, input: &mut LocInput) {
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

pub(super) fn new_form_ui(ui: &mut egui::Ui, f: &mut NewForm, snapshot: &Snapshot) {
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

pub(super) fn attach_form_ui(ui: &mut egui::Ui, f: &mut AttachForm) {
    ui.label(t(
        "Lägg till en fil till förfrågan, till exempel en CAD-fil eller ett lastfall. Den syns för alla i laget.",
        "Add a file to the request, for example a CAD file or a load case. Everyone on the team can see it.",
    ));
    ui.add_space(6.0);
    loc_input_ui(ui, "attach_kind", &mut f.location);
}

pub(super) fn finish_form_ui(ui: &mut egui::Ui, f: &mut FinishForm) {
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

pub(super) fn note_form_ui(ui: &mut egui::Ui, f: &mut NoteForm) {
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

impl App {
    /// Opens the form for a new request, optionally filled in for a department and a part.
    pub(super) fn open_new_form(&mut self, department: Option<Uuid>, part: Option<Uuid>) {
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

    pub(super) fn form_ui(&mut self, ctx: &egui::Context) {
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
                        Form::Attach(f) => attach_form_ui(ui, f),
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
}
