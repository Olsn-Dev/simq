//! The queue: tabs, filters, the table of requests and the open request.

use super::*;

/// The filters above the queue. They are local to this app and never synced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Filters {
    /// `Some(None)` means requests without a department.
    pub(super) department: Option<Option<Uuid>>,
    pub(super) sim_type: Option<String>,
    pub(super) priority: Option<u8>,
    /// Requester or assignee.
    pub(super) person: Option<String>,
    pub(super) mine: bool,
    pub(super) unread: bool,
    /// Active requests with a deadline that has passed or is close.
    pub(super) due_soon: bool,
}

impl Filters {
    pub(super) fn any(&self) -> bool {
        *self != Filters::default()
    }

    pub(super) fn matches(&self, r: &Request, me: &str, unread: &HashSet<Uuid>) -> bool {
        let is = |name: &str| r.requester == name || r.assignee == name;
        self.department.is_none_or(|d| r.department == d)
            && self.sim_type.as_ref().is_none_or(|t| r.sim_type == *t)
            && self.priority.is_none_or(|p| r.priority == p)
            && self.person.as_ref().is_none_or(|p| is(p))
            && (!self.mine || is(me))
            && (!self.unread || unread.contains(&r.request_id))
            && (!self.due_soon || is_due_soon(r, Local::now().date_naive()))
    }
}

/// A column the queue can be sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum SortCol {
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

/// A column of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Col {
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

    pub(super) fn title(self) -> &'static str {
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
    pub(super) fn width(self) -> f32 {
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

    pub(super) fn sort(self) -> Option<SortCol> {
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
    pub(super) fn rank(self) -> u8 {
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
    pub(super) fn fitting(available: f32) -> Vec<Col> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(super) enum Tab {
    #[default]
    Active,
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
    All,
    /// Finished requests that ended more than `ARCHIVE_DAYS` ago.
    Archive,
}

/// Finished requests move to the archive this many days after they ended,
/// so that the other tabs stay short. Nothing is deleted.
pub(super) const ARCHIVE_DAYS: u64 = 30;

/// True for a request that has been finished for more than `ARCHIVE_DAYS`.
pub(super) fn is_archived(r: &Request, now_ms: u64) -> bool {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    !r.status.is_active()
        && r.finished_at
            .is_some_and(|at| now_ms.saturating_sub(at) > ARCHIVE_DAYS * DAY)
}

/// Days ahead that a deadline counts as close.
pub(super) const DUE_SOON_DAYS: i64 = 3;

/// An active request whose deadline has passed or is at most
/// `DUE_SOON_DAYS` away.
pub(super) fn is_due_soon(r: &Request, today: NaiveDate) -> bool {
    r.status.is_active()
        && NaiveDate::parse_from_str(&r.deadline, "%Y-%m-%d")
            .is_ok_and(|deadline| (deadline - today).num_days() <= DUE_SOON_DAYS)
}

impl Tab {
    pub(super) const ALL: [Tab; 8] = [
        Tab::Active,
        Tab::Queued,
        Tab::Running,
        Tab::Done,
        Tab::Failed,
        Tab::Cancelled,
        Tab::All,
        Tab::Archive,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Tab::Active => t("Aktiva", "Active"),
            Tab::Queued => t("Köade", "Queued"),
            Tab::Running => t("Pågår", "In progress"),
            Tab::Done => t("Klara", "Done"),
            Tab::Failed => t("Misslyckade", "Failed"),
            Tab::Cancelled => t("Avbrutna", "Cancelled"),
            Tab::All => t("Alla", "All"),
            Tab::Archive => t("Arkiv", "Archive"),
        }
    }

    /// Archived requests only show in the archive, and in "All" while searching.
    pub(super) fn matches(self, r: &Request, now_ms: u64, searching: bool) -> bool {
        let status = r.status;
        if is_archived(r, now_ms) {
            return self == Tab::Archive || (self == Tab::All && searching);
        }
        match self {
            Tab::Active => status.is_active(),
            Tab::Queued => status == Status::Queued,
            Tab::Running => status == Status::Running,
            Tab::Done => status == Status::Done,
            Tab::Failed => status == Status::Failed,
            Tab::Cancelled => status == Status::Cancelled,
            Tab::All => true,
            Tab::Archive => false,
        }
    }
}

pub(super) fn matches_search(request: &Request, needle: &str, snapshot: &Snapshot) -> bool {
    needle.is_empty()
        || request.title.to_lowercase().contains(needle)
        || request.requester.to_lowercase().contains(needle)
        || request.assignee.to_lowercase().contains(needle)
        || request.sim_type.to_lowercase().contains(needle)
        || find_department(snapshot, request.department)
            .is_some_and(|d| d.name.to_lowercase().contains(needle))
        || find_part(snapshot, request.part).is_some_and(|p| p.name.to_lowercase().contains(needle))
}

pub(super) fn sort_cmp(col: SortCol, a: &Request, b: &Request, snapshot: &Snapshot) -> CmpOrdering {
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

pub(super) fn is_overdue(request: &Request, today: NaiveDate) -> bool {
    request.status.is_active()
        && NaiveDate::parse_from_str(&request.deadline, "%Y-%m-%d")
            .map(|deadline| deadline < today)
            .unwrap_or(false)
}

/// A table cell with one line of text that is clipped if it does not fit.
pub(super) fn cell(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(one_line(text)).truncate());
}

/// A label and a value on one row, with the labels lined up in a column.
pub(super) fn text_field(ui: &mut egui::Ui, label: &str, value: &str) {
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

/// Shows a location. Only http(s) values become clickable links, everything else is plain text.
pub(super) fn location_field(
    ui: &mut egui::Ui,
    icons: &theme::Icons,
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
                            if ui
                                .add(
                                    icons
                                        .button(
                                            ui,
                                            theme::Icon::Download,
                                            t("Ladda ner", "Download"),
                                            "",
                                        )
                                        .small(),
                                )
                                .clicked()
                            {
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
                    if ui
                        .add(
                            icons
                                .button(ui, theme::Icon::Copy, t("Kopiera", "Copy"), "")
                                .small(),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(location.value.clone());
                    }
                });
            }
        });
    });
}

/// Quick link in the table row for done requests.
pub(super) fn quick_link(
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

impl App {
    /// The requests that pass the search and the filters, and the tab if one is given.
    pub(super) fn filtered<'a>(
        &self,
        snapshot: &'a Snapshot,
        tab: Option<Tab>,
    ) -> Vec<&'a Request> {
        let needle = self.search.trim().to_lowercase();
        let now = crate::sync::now_ms();
        snapshot
            .requests
            .iter()
            .filter(|r| {
                tab.is_none_or(|tab| tab.matches(r, now, !needle.is_empty()))
                    && matches_search(r, &needle, snapshot)
                    && self
                        .filters
                        .matches(r, &self.config.display_name, &snapshot.unread)
            })
            .collect()
    }

    pub(super) fn sort_rows(&self, rows: &mut [&Request], snapshot: &Snapshot) {
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

    /// A line that points out requests whose deadline has passed or is close.
    pub(super) fn deadline_reminder_ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if self.filters.due_soon {
            return;
        }
        let today = Local::now().date_naive();
        let me = &self.config.display_name;
        let due: Vec<&Request> = snapshot
            .requests
            .iter()
            .filter(|r| is_due_soon(r, today))
            .collect();
        if due.is_empty() {
            return;
        }
        let overdue = due.iter().filter(|r| is_overdue(r, today)).count();
        let mine = due
            .iter()
            .filter(|r| r.requester == *me || r.assignee == *me)
            .count();
        let text = match (is_en(), overdue) {
            (true, 0) => format!(
                "{} request(s) have a deadline within {DUE_SOON_DAYS} days",
                due.len()
            ),
            (true, _) => format!(
                "{} request(s) have a deadline within {DUE_SOON_DAYS} days, {overdue} of them overdue",
                due.len()
            ),
            (false, 0) => format!(
                "{} förfrågan/förfrågningar har deadline inom {DUE_SOON_DAYS} dagar",
                due.len()
            ),
            (false, _) => format!(
                "{} förfrågan/förfrågningar har deadline inom {DUE_SOON_DAYS} dagar, varav {overdue} försenade",
                due.len()
            ),
        };
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            theme::dot(ui, ui.visuals().warn_fg_color);
            ui.label(text);
            if mine > 0 {
                ui.weak(if is_en() {
                    format!("({mine} yours)")
                } else {
                    format!("({mine} dina)")
                });
            }
            if ui.small_button(t("Visa dem", "Show them")).clicked() {
                self.filters.due_soon = true;
                self.tab = Tab::Active;
            }
        });
    }

    /// The row of filters above the queue.
    pub(super) fn filter_ui(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
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
            ui.toggle_value(&mut self.filters.due_soon, t("Snart deadline", "Due soon"))
                .on_hover_text(if is_en() {
                    format!("Active requests whose deadline has passed or is at most {DUE_SOON_DAYS} days away")
                } else {
                    format!("Aktiva ärenden vars deadline har passerat eller är högst {DUE_SOON_DAYS} dagar bort")
                });
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

    pub(super) fn table_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
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

    pub(super) fn detail_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
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
                                    if theme::named(
                                        ui.add(icons.button(ui, theme::Icon::Close, "", "×")),
                                        t("Stäng", "Close"),
                                    )
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
                                    &icons,
                                    t("Rapport", "Report"),
                                    request.report.as_ref(),
                                    actions,
                                );
                                location_field(
                                    ui,
                                    &icons,
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
                        location_field(
                            ui,
                            &icons,
                            t("Indata", "Input"),
                            request.input.as_ref(),
                            actions,
                        );

                        theme::section(ui, t("Beskrivning", "Description"));
                        if request.description.trim().is_empty() {
                            ui.weak(t("Ingen beskrivning", "No description"));
                        } else {
                            ui.label(clean_text(&request.description));
                        }

                        theme::section(ui, t("Fler filer", "More files"));
                        if request.attachments.is_empty() {
                            ui.weak(t("Inga fler filer", "No more files"));
                        }
                        for attachment in &request.attachments {
                            // Who added it, as the label in front of the file.
                            let label = truncate_chars(&one_line(&attachment.by), 12);
                            location_field(ui, &icons, &label, Some(&attachment.location), actions);
                        }
                        let room = request.attachments.len() < MAX_ATTACHMENTS;
                        if ui
                            .add_enabled(
                                can_write && room,
                                icons
                                    .button(
                                        ui,
                                        theme::Icon::Plus,
                                        t("Lägg till fil…", "Add file…"),
                                        "",
                                    )
                                    .small(),
                            )
                            .on_disabled_hover_text(if room {
                                why.to_string()
                            } else if is_en() {
                                format!("A request can have at most {MAX_ATTACHMENTS} more files")
                            } else {
                                format!("En förfrågan kan ha högst {MAX_ATTACHMENTS} fler filer")
                            })
                            .clicked()
                        {
                            actions.push(UiAction::OpenAttach(id));
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
}
