//! The overview: a drawing of the car where every department owns the parts
//! of the car it is responsible for. A department works like a folder of
//! parts, and the colour of its regions shows how far the parts have come.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind,
};
use uuid::Uuid;

use super::car::{self, Car, Outline, Region};
use super::{App, Confirm, ConfirmWhat, Pending, UiAction};
use crate::i18n::{is_en, t};
use crate::model::{
    clean_text, one_line, progress, truncate_chars, Department, Part, PartStatus, COLOR_COUNT,
    MAX_DEPT_NAME, MAX_PART_NAME, MAX_TEXT,
};
use crate::ops::NewAction;
use crate::sync::Snapshot;
use crate::theme;

/// What the overview remembers between frames.
#[derive(Default)]
pub(super) struct BoardUi {
    selected: Option<Uuid>,
    new_dept: String,
    new_part: String,
    text_edit: Option<TextEdit>,
    place: Option<Place>,
    /// A department that was just created: as soon as it has been saved the
    /// user gets to choose its regions.
    place_after: Option<Uuid>,
    /// Where the car was drawn in the last frame.
    canvas: Option<Rect>,
    car: Car,
    /// The line art of the car, loaded when the app starts.
    pub(super) drawing: Option<egui::TextureHandle>,
}

/// A name or note being edited in place.
struct TextEdit {
    target: Uuid,
    what: EditWhat,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditWhat {
    DeptName,
    PartName,
    PartNote,
}

/// The regions of a department while the user is choosing them on the car.
struct Place {
    dept: Uuid,
    regions: Vec<String>,
}

/// What the user did in the overview during a frame.
enum BoardAction {
    Select(Option<Uuid>),
    AddDept(String),
    AddSuggested,
    SetDept(Department),
    DeleteDept(Uuid),
    AddPart(Uuid, String),
    SetPart(Part),
    DeletePart(Uuid),
    Place(Uuid),
    SavePlace,
    CancelPlace,
    EditText(Uuid, EditWhat, String),
    SaveText,
    CancelText,
}

/// Suggested departments: (Swedish, English, regions of the car).
const SUGGESTED: [(&str, &str, &[&str]); 5] = [
    ("Aero", "Aero", &["front_wing", "rear_wing", "floor"]),
    ("Chassi", "Chassis", &["monocoque", "nose"]),
    (
        "Hjulupphängning",
        "Suspension",
        &[
            "front_suspension",
            "rear_suspension",
            "front_wheels",
            "rear_wheels",
        ],
    ),
    ("Drivlina", "Powertrain", &["engine_cover"]),
    ("Elektronik", "Electronics", &["sidepods"]),
];

/// Maps between the drawing's units and the screen.
struct Map {
    origin: Pos2,
    scale: f32,
}

impl Map {
    fn pt(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.origin.x + x * self.scale,
            self.origin.y + y * self.scale,
        )
    }

    /// A screen position in the drawing's units.
    fn canvas(&self, p: Pos2) -> (f32, f32) {
        (
            (p.x - self.origin.x) / self.scale,
            (p.y - self.origin.y) / self.scale,
        )
    }
}

fn parts_of(snapshot: &Snapshot, dept: Uuid) -> impl Iterator<Item = &Part> {
    snapshot.parts.iter().filter(move |p| p.department == dept)
}

/// (progress, parts done, parts in total) for a department.
fn dept_progress(snapshot: &Snapshot, dept: Uuid) -> (Option<f32>, usize, usize) {
    let statuses: Vec<PartStatus> = parts_of(snapshot, dept).map(|p| p.status).collect();
    let done = statuses.iter().filter(|s| **s == PartStatus::Done).count();
    (progress(statuses.iter().copied()), done, statuses.len())
}

fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// The department that owns a region. If two departments claim the same
/// region (they were placed at the same moment), the older one has it.
fn owner<'a>(snapshot: &'a Snapshot, region: &str) -> Option<&'a Department> {
    snapshot
        .departments
        .iter()
        .find(|d| d.regions.iter().any(|r| r == region))
}

/// The regions of the drawing that a department has, in the drawing's order.
fn regions_of<'a>(car: &'a Car, dept: &'a Department) -> impl Iterator<Item = &'a Region> {
    car.regions
        .iter()
        .filter(move |r| dept.regions.iter().any(|id| id == r.id))
}

impl App {
    pub(super) fn overview_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        let snapshot = Arc::clone(&self.snapshot);
        let can_write = self.can_write();
        let why = self.why_read_only();
        let live = |id: Uuid| snapshot.departments.iter().any(|d| d.id == id);
        // Forget what pointed at a department that is no longer there.
        if self.board.selected.is_some_and(|id| !live(id)) {
            self.board.selected = None;
        }
        if self.board.place.as_ref().is_some_and(|e| !live(e.dept)) {
            self.board.place = None;
        }
        // A new department goes straight to choosing its regions.
        if let Some(id) = self.board.place_after.filter(|id| live(*id)) {
            self.board.place_after = None;
            self.board.place = Some(Place {
                dept: id,
                regions: Vec::new(),
            });
        }

        let mut out = Vec::new();
        egui::SidePanel::right("board_side")
            .resizable(true)
            .default_width(380.0)
            .min_width(300.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        side_ui(
                            ui,
                            &mut self.board,
                            &snapshot,
                            can_write,
                            why,
                            &mut out,
                            actions,
                        )
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            canvas_ui(ui, &mut self.board, &snapshot, can_write, &mut out);
        });
        for action in out {
            self.apply_board(&snapshot, action);
        }
    }

    fn apply_board(&mut self, snapshot: &Snapshot, action: BoardAction) {
        let dept = |id: Uuid| snapshot.departments.iter().find(|d| d.id == id);
        let part = |id: Uuid| snapshot.parts.iter().find(|p| p.id == id);
        let set_dept = |d: &Department| NewAction::DeptSet {
            name: d.name.clone(),
            color: d.color,
            zones: d.zones.clone(),
            regions: d.regions.clone(),
        };
        let set_part = |p: &Part| NewAction::PartSet {
            department: p.department,
            name: p.name.clone(),
            status: p.status,
            note: p.note.clone(),
        };
        match action {
            BoardAction::Select(id) => {
                self.board.selected = id;
                self.board.text_edit = None;
                self.board.new_part.clear();
            }
            BoardAction::AddDept(name) => {
                let id = Uuid::new_v4();
                // The first colour that no other department uses.
                let color = (0..COLOR_COUNT)
                    .find(|c| snapshot.departments.iter().all(|d| d.color != *c))
                    .unwrap_or((snapshot.departments.len() % usize::from(COLOR_COUNT)) as u8);
                self.submit(
                    Some(id),
                    NewAction::DeptSet {
                        name,
                        color,
                        zones: Vec::new(),
                        regions: Vec::new(),
                    },
                    Vec::new(),
                    Pending::Other,
                );
                self.board.new_dept.clear();
                self.board.selected = Some(id);
                self.board.place_after = Some(id);
            }
            BoardAction::AddSuggested => {
                let items = SUGGESTED
                    .iter()
                    .enumerate()
                    .map(|(i, (sv, en, regions))| {
                        (
                            Uuid::new_v4(),
                            NewAction::DeptSet {
                                name: t(sv, en).to_string(),
                                color: i as u8,
                                zones: Vec::new(),
                                regions: regions.iter().map(|id| id.to_string()).collect(),
                            },
                        )
                    })
                    .collect();
                self.submit_many(items);
            }
            BoardAction::SetDept(d) => {
                self.submit(Some(d.id), set_dept(&d), Vec::new(), Pending::Other)
            }
            BoardAction::DeleteDept(id) => {
                if let Some(d) = dept(id) {
                    let parts = parts_of(snapshot, id).count();
                    self.confirm = Some(Confirm {
                        title: format!(
                            "{}: {}",
                            t("Ta bort avdelning", "Delete department"),
                            truncate_chars(&one_line(&d.name), 40)
                        ),
                        text: if is_en() {
                            format!(
                                "The department and its {parts} part(s) disappear for everyone on the team. This cannot be undone. Requests that belong to the department stay in the queue."
                            )
                        } else {
                            format!(
                                "Avdelningen och dess {parts} del(ar) försvinner för alla i laget. Det går inte att ångra. Ärenden som hör till avdelningen ligger kvar i kön."
                            )
                        },
                        button: t("Ta bort avdelningen", "Delete the department").to_string(),
                        what: ConfirmWhat::Submit(id, Box::new(NewAction::DeptDelete)),
                    });
                }
            }
            BoardAction::AddPart(department, name) => {
                self.submit(
                    Some(Uuid::new_v4()),
                    NewAction::PartSet {
                        department,
                        name,
                        status: PartStatus::NotStarted,
                        note: String::new(),
                    },
                    Vec::new(),
                    Pending::Other,
                );
                self.board.new_part.clear();
            }
            BoardAction::SetPart(p) => {
                self.submit(Some(p.id), set_part(&p), Vec::new(), Pending::Other)
            }
            BoardAction::DeletePart(id) => {
                if let Some(p) = part(id) {
                    self.confirm = Some(Confirm {
                        title: format!(
                            "{}: {}",
                            t("Ta bort del", "Delete part"),
                            truncate_chars(&one_line(&p.name), 40)
                        ),
                        text: t(
                            "Delen försvinner för alla i laget. Det går inte att ångra.",
                            "The part disappears for everyone on the team. This cannot be undone.",
                        )
                        .to_string(),
                        button: t("Ta bort delen", "Delete the part").to_string(),
                        what: ConfirmWhat::Submit(id, Box::new(NewAction::PartDelete)),
                    });
                }
            }
            BoardAction::Place(id) => {
                if let Some(d) = dept(id) {
                    self.board.selected = Some(id);
                    self.board.place = Some(Place {
                        dept: id,
                        regions: d.regions.clone(),
                    });
                }
            }
            BoardAction::SavePlace => {
                if let Some(place) = self.board.place.take() {
                    if let Some(d) = dept(place.dept) {
                        if d.regions != place.regions {
                            let changed = Department {
                                regions: place.regions,
                                ..d.clone()
                            };
                            self.submit(Some(d.id), set_dept(&changed), Vec::new(), Pending::Other);
                        }
                    }
                }
            }
            BoardAction::CancelPlace => self.board.place = None,
            BoardAction::EditText(target, what, text) => {
                self.board.text_edit = Some(TextEdit { target, what, text });
            }
            BoardAction::CancelText => self.board.text_edit = None,
            BoardAction::SaveText => {
                let Some(edit) = self.board.text_edit.take() else {
                    return;
                };
                let text = edit.text.trim().to_string();
                match edit.what {
                    EditWhat::DeptName => {
                        if let Some(d) = dept(edit.target).filter(|d| d.name != text) {
                            let changed = Department {
                                name: text,
                                ..d.clone()
                            };
                            self.submit(Some(d.id), set_dept(&changed), Vec::new(), Pending::Other);
                        }
                    }
                    EditWhat::PartName => {
                        if let Some(p) = part(edit.target).filter(|p| p.name != text) {
                            let changed = Part {
                                name: text,
                                ..p.clone()
                            };
                            self.submit(Some(p.id), set_part(&changed), Vec::new(), Pending::Other);
                        }
                    }
                    EditWhat::PartNote => {
                        if let Some(p) = part(edit.target).filter(|p| p.note != text) {
                            let changed = Part {
                                note: text,
                                ..p.clone()
                            };
                            self.submit(Some(p.id), set_part(&changed), Vec::new(), Pending::Other);
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The side panel: departments and their parts
// ---------------------------------------------------------------------------

fn side_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    snapshot: &Snapshot,
    can_write: bool,
    why: &str,
    out: &mut Vec<BoardAction>,
    actions: &mut Vec<UiAction>,
) {
    let selected = board
        .selected
        .and_then(|id| snapshot.departments.iter().find(|d| d.id == id));
    match selected {
        Some(dept) => dept_ui(ui, board, snapshot, dept, can_write, why, out, actions),
        None => dept_list_ui(ui, board, snapshot, can_write, why, out),
    }
}

/// The inline editor for a name or a note. Returns true if it was shown.
fn text_edit_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    target: Uuid,
    what: EditWhat,
    out: &mut Vec<BoardAction>,
) -> bool {
    let Some(edit) = board
        .text_edit
        .as_mut()
        .filter(|e| e.target == target && e.what == what)
    else {
        return false;
    };
    let (limit, required) = match what {
        EditWhat::DeptName => (MAX_DEPT_NAME, true),
        EditWhat::PartName => (MAX_PART_NAME, true),
        EditWhat::PartNote => (MAX_TEXT, false),
    };
    ui.horizontal(|ui| {
        let ok = !required || !edit.text.trim().is_empty();
        if ui.small_button(t("Avbryt", "Cancel")).clicked() {
            out.push(BoardAction::CancelText);
        }
        let save = ui
            .add_enabled(ok, egui::Button::new(t("Spara", "Save")).small())
            .clicked();
        let response = ui.add(
            egui::TextEdit::singleline(&mut edit.text)
                .char_limit(limit)
                .desired_width(f32::INFINITY),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (save || enter) && ok {
            out.push(BoardAction::SaveText);
        }
    });
    true
}

fn dept_list_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    snapshot: &Snapshot,
    can_write: bool,
    why: &str,
    out: &mut Vec<BoardAction>,
) {
    let dark = ui.visuals().dark_mode;
    ui.add_space(2.0);
    ui.label(
        RichText::new(t("Avdelningar", "Departments"))
            .heading()
            .strong(),
    );
    let all = progress(snapshot.parts.iter().map(|p| p.status));
    ui.horizontal(|ui| {
        ui.weak(t("Hela bilen", "The whole car"));
        theme::progress_bar(ui, all, 120.0);
        ui.label(theme::percent_label(all));
    });
    ui.add_space(8.0);

    if snapshot.departments.is_empty() {
        ui.label(t(
            "En avdelning fungerar som en mapp: du ger den ett namn, markerar var på bilen den hör hemma och lägger in dess delar.",
            "A department works like a folder: you give it a name, mark where on the car it belongs and add its parts.",
        ));
        ui.add_space(6.0);
        if ui
            .add_enabled(
                can_write,
                egui::Button::new(t("Lägg till förslag", "Add suggestions")),
            )
            .on_hover_text(t(
                "Skapar Aero, Chassi, Hjulupphängning, Drivlina och Elektronik, redan placerade på bilen. Allt kan ändras efteråt.",
                "Creates Aero, Chassis, Suspension, Powertrain and Electronics, already placed on the car. Everything can be changed afterwards.",
            ))
            .on_disabled_hover_text(why)
            .clicked()
        {
            out.push(BoardAction::AddSuggested);
        }
        ui.add_space(8.0);
    }

    for dept in &snapshot.departments {
        let (progress, done, total) = dept_progress(snapshot, dept.id);
        let response = theme::card(ui)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::dot(ui, theme::dept_color(dept.color, dark));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(theme::percent_label(progress));
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(one_line(&dept.name)).strong())
                                    .truncate()
                                    .selectable(false),
                            );
                        });
                    });
                });
                ui.horizontal(|ui| {
                    theme::progress_bar(ui, progress, 150.0);
                    ui.weak(if total == 0 {
                        t("Inga delar än", "No parts yet").to_string()
                    } else if is_en() {
                        format!("{done} of {total} parts done")
                    } else {
                        format!("{done} av {total} delar klara")
                    });
                });
                if regions_of(&board.car, dept).next().is_none() {
                    ui.weak(t("Inte placerad på bilen", "Not placed on the car"));
                }
            })
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            out.push(BoardAction::Select(Some(dept.id)));
        }
        ui.add_space(2.0);
    }

    theme::section(ui, t("Ny avdelning", "New department"));
    ui.horizontal(|ui| {
        let name = board.new_dept.trim().to_string();
        let taken = snapshot
            .departments
            .iter()
            .any(|d| d.name.trim().to_lowercase() == name.to_lowercase());
        let add = ui
            .add_enabled(
                can_write && !name.is_empty() && !taken,
                egui::Button::new(t("Lägg till", "Add")),
            )
            .on_disabled_hover_text(if taken {
                t(
                    "Det finns redan en avdelning med det namnet",
                    "There is already a department with that name",
                )
            } else if name.is_empty() {
                t("Skriv ett namn först", "Type a name first")
            } else {
                why
            })
            .clicked();
        let response = ui.add(
            egui::TextEdit::singleline(&mut board.new_dept)
                .hint_text(t("t.ex. Aero", "e.g. Aero"))
                .char_limit(MAX_DEPT_NAME)
                .desired_width(f32::INFINITY),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (add || enter) && can_write && !name.is_empty() && !taken {
            out.push(BoardAction::AddDept(name));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn dept_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    snapshot: &Snapshot,
    dept: &Department,
    can_write: bool,
    why: &str,
    out: &mut Vec<BoardAction>,
    actions: &mut Vec<UiAction>,
) {
    let dark = ui.visuals().dark_mode;
    ui.add_space(2.0);
    if ui
        .button(t("⏴ Alla avdelningar", "⏴ All departments"))
        .clicked()
    {
        out.push(BoardAction::Select(None));
    }
    ui.add_space(6.0);
    if !text_edit_ui(ui, board, dept.id, EditWhat::DeptName, out) {
        ui.horizontal(|ui| {
            theme::dot(ui, theme::dept_color(dept.color, dark));
            ui.add(egui::Label::new(RichText::new(one_line(&dept.name)).heading().strong()).wrap());
        });
    }
    let (progress, done, total) = dept_progress(snapshot, dept.id);
    ui.horizontal(|ui| {
        theme::progress_bar(ui, progress, 150.0);
        ui.label(theme::percent_label(progress));
        if total > 0 {
            ui.weak(if is_en() {
                format!("{done} of {total} parts done")
            } else {
                format!("{done} av {total} delar klara")
            });
        }
    });
    let placed: Vec<&str> = regions_of(&board.car, dept).map(|r| r.label()).collect();
    ui.add(
        egui::Label::new(
            RichText::new(if placed.is_empty() {
                t("Inte placerad på bilen", "Not placed on the car").to_string()
            } else {
                format!("{}: {}", t("På bilen", "On the car"), placed.join(", "))
            })
            .size(12.5)
            .color(ui.visuals().weak_text_color()),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    ui.add_enabled_ui(can_write, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(t("Placera på bilen", "Place on the car"))
                .on_hover_text(t(
                    "Välj vilka delar av bilen som hör till avdelningen",
                    "Choose which parts of the car belong to the department",
                ))
                .on_disabled_hover_text(why)
                .clicked()
            {
                out.push(BoardAction::Place(dept.id));
            }
            if ui.button(t("Byt namn", "Rename")).clicked() {
                out.push(BoardAction::EditText(
                    dept.id,
                    EditWhat::DeptName,
                    dept.name.clone(),
                ));
            }
            ui.menu_button(t("Färg", "Colour"), |ui| {
                ui.set_max_width(136.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for color in 0..COLOR_COUNT {
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::click());
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(5),
                            theme::dept_color(color, dark),
                        );
                        if color == dept.color {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                CornerRadius::same(7),
                                Stroke::new(1.5_f32, ui.visuals().strong_text_color()),
                                StrokeKind::Outside,
                            );
                        }
                        if response.clicked() {
                            if color != dept.color {
                                out.push(BoardAction::SetDept(Department {
                                    color,
                                    ..dept.clone()
                                }));
                            }
                            ui.close_menu();
                        }
                    }
                });
            });
            if ui.button(t("Ta bort…", "Delete…")).clicked() {
                out.push(BoardAction::DeleteDept(dept.id));
            }
        });
    });

    theme::section(ui, t("Delar", "Parts"));
    ui.add_space(2.0);
    let parts: Vec<&Part> = parts_of(snapshot, dept.id).collect();
    if parts.is_empty() {
        ui.weak(t(
            "Inga delar än. Lägg till den första här under.",
            "No parts yet. Add the first one below.",
        ));
    }
    for part in parts {
        if text_edit_ui(ui, board, part.id, EditWhat::PartName, out) {
            continue;
        }
        let open = snapshot
            .requests
            .iter()
            .filter(|r| r.part == Some(part.id) && r.status.is_active())
            .count();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(can_write, |ui| {
                    ui.menu_button("…", |ui| {
                        if ui.button(t("Byt namn", "Rename")).clicked() {
                            out.push(BoardAction::EditText(
                                part.id,
                                EditWhat::PartName,
                                part.name.clone(),
                            ));
                            ui.close_menu();
                        }
                        if ui.button(t("Anteckning…", "Note…")).clicked() {
                            out.push(BoardAction::EditText(
                                part.id,
                                EditWhat::PartNote,
                                part.note.clone(),
                            ));
                            ui.close_menu();
                        }
                        if ui
                            .button(t("Ny simuleringsförfrågan", "New simulation request"))
                            .clicked()
                        {
                            actions.push(UiAction::NewRequest(Some(dept.id), Some(part.id)));
                            ui.close_menu();
                        }
                        if ui.button(t("Ta bort…", "Delete…")).clicked() {
                            out.push(BoardAction::DeletePart(part.id));
                            ui.close_menu();
                        }
                    });
                    let mut status = part.status;
                    egui::ComboBox::from_id_salt(("part_status", part.id))
                        .selected_text(
                            RichText::new(status.label())
                                .color(theme::part_status_color(status, dark)),
                        )
                        .width(112.0)
                        .show_ui(ui, |ui| {
                            for choice in PartStatus::ALL {
                                ui.selectable_value(&mut status, choice, choice.label());
                            }
                        });
                    if status != part.status {
                        out.push(BoardAction::SetPart(Part {
                            status,
                            ..part.clone()
                        }));
                    }
                });
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(one_line(&part.name)).truncate())
                        .on_hover_text(if is_en() {
                            format!("Last changed by {}", one_line(&part.last_by))
                        } else {
                            format!("Senast ändrad av {}", one_line(&part.last_by))
                        });
                });
            });
        });
        if !text_edit_ui(ui, board, part.id, EditWhat::PartNote, out)
            && !part.note.trim().is_empty()
        {
            ui.indent(("part_note", part.id), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(clean_text(&part.note))
                            .size(12.5)
                            .color(ui.visuals().weak_text_color()),
                    )
                    .wrap(),
                );
            });
        }
        if open > 0 {
            ui.indent(("part_open", part.id), |ui| {
                ui.label(
                    RichText::new(if is_en() {
                        format!("{open} open simulation request(s)")
                    } else {
                        format!("{open} öppna simuleringsförfrågningar")
                    })
                    .size(12.5)
                    .color(theme::status_color(crate::model::Status::Running, dark)),
                );
            });
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let name = board.new_part.trim().to_string();
        let add = ui
            .add_enabled(
                can_write && !name.is_empty(),
                egui::Button::new(t("Lägg till del", "Add part")),
            )
            .on_disabled_hover_text(if name.is_empty() {
                t("Skriv ett namn först", "Type a name first")
            } else {
                why
            })
            .clicked();
        let response = ui.add(
            egui::TextEdit::singleline(&mut board.new_part)
                .hint_text(t("t.ex. Bakvinge", "e.g. Rear wing"))
                .char_limit(MAX_PART_NAME)
                .desired_width(f32::INFINITY),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (add || enter) && can_write && !name.is_empty() {
            out.push(BoardAction::AddPart(dept.id, name));
        }
    });

    theme::section(ui, t("Simuleringar", "Simulations"));
    ui.add_space(2.0);
    let requests: Vec<_> = snapshot
        .requests
        .iter()
        .filter(|r| r.department == Some(dept.id) && r.status.is_active())
        .collect();
    if requests.is_empty() {
        ui.weak(t(
            "Inga öppna förfrågningar från avdelningen.",
            "No open requests from the department.",
        ));
    }
    for request in requests {
        ui.horizontal(|ui| {
            theme::status_pill(ui, request.status);
            if ui
                .link(truncate_chars(&one_line(&request.title), 60))
                .on_hover_text(t("Visa i kön", "Show in the queue"))
                .clicked()
            {
                actions.push(UiAction::GoToRequest(request.request_id));
            }
        });
    }
    ui.add_space(6.0);
    if ui
        .add_enabled(
            can_write,
            egui::Button::new(t(
                "+ Ny förfrågan för avdelningen",
                "+ New request for the department",
            )),
        )
        .on_disabled_hover_text(why)
        .clicked()
    {
        actions.push(UiAction::NewRequest(Some(dept.id), None));
    }
    ui.add_space(10.0);
}

// ---------------------------------------------------------------------------
// The car
// ---------------------------------------------------------------------------

/// How strongly a region is drawn.
struct Look {
    fill: u8,
    line: f32,
    line_alpha: u8,
    glow: bool,
}

const IDLE: Look = Look {
    fill: 13,
    line: 1.0,
    line_alpha: 105,
    glow: false,
};
const HOVERED: Look = Look {
    fill: 44,
    line: 2.0,
    line_alpha: 255,
    glow: true,
};
const SELECTED: Look = Look {
    fill: 58,
    line: 2.5,
    line_alpha: 255,
    glow: false,
};

fn paint_outline(
    painter: &egui::Painter,
    map: &Map,
    outline: &Outline,
    color: Color32,
    look: &Look,
) {
    let points: Vec<Pos2> = outline.points.iter().map(|(x, y)| map.pt(*x, *y)).collect();
    if look.fill > 0 {
        // The same opacity looks stronger on a dark background.
        let dark = painter.ctx().style().visuals.dark_mode;
        let alpha = if dark {
            (u16::from(look.fill) * 6 / 10) as u8
        } else {
            look.fill
        };
        let fill = with_alpha(color, alpha.max(1));
        let mut mesh = egui::Mesh::default();
        for point in &points {
            mesh.colored_vertex(*point, fill);
        }
        for [a, b, c] in &outline.triangles {
            mesh.add_triangle(*a, *b, *c);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
    if look.glow {
        for (extra, alpha) in [(7.0, 34), (3.5, 70)] {
            painter.add(egui::Shape::closed_line(
                points.clone(),
                Stroke::new(look.line + extra, with_alpha(color, alpha)),
            ));
        }
    }
    painter.add(egui::Shape::closed_line(
        points,
        Stroke::new(look.line, with_alpha(color, look.line_alpha)),
    ));
}

fn paint_region(painter: &egui::Painter, map: &Map, region: &Region, color: Color32, look: &Look) {
    for outline in &region.outlines {
        paint_outline(painter, map, outline, color, look);
    }
}

/// A small name tag on the drawing.
fn tag(painter: &egui::Painter, at: Pos2, text: &str, visuals: &egui::Visuals) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(12.5),
        visuals.strong_text_color(),
    );
    let rect = Rect::from_center_size(at, galley.size() + egui::vec2(14.0, 6.0));
    painter.rect(
        rect,
        CornerRadius::same(9),
        visuals.window_fill,
        visuals.window_stroke,
        StrokeKind::Outside,
    );
    painter.galley(
        rect.center() - galley.size() / 2.0,
        galley,
        visuals.strong_text_color(),
    );
}

fn canvas_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    snapshot: &Snapshot,
    can_write: bool,
    out: &mut Vec<BoardAction>,
) {
    let dark = ui.visuals().dark_mode;
    if let Some(place) = &board.place {
        let name = snapshot
            .departments
            .iter()
            .find(|d| d.id == place.dept)
            .map(|d| truncate_chars(&one_line(&d.name), 40))
            .unwrap_or_default();
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(name).strong());
                ui.label(t(
                    "Klicka på de delar av bilen som hör till avdelningen. Klicka igen för att ta bort en.",
                    "Click the parts of the car that belong to the department. Click again to remove one.",
                ));
                if theme::add_primary(ui, can_write, t("Spara", "Save")).clicked() {
                    out.push(BoardAction::SavePlace);
                }
                if ui.button(t("Avbryt", "Cancel")).clicked() {
                    out.push(BoardAction::CancelPlace);
                }
            });
        });
        ui.add_space(4.0);
    }

    let mut available = ui.available_size();
    available.y = (available.y - 26.0).max(60.0);
    let scale = (available.x / car::W).min(available.y / car::H).max(0.1);
    let (outer, _) = ui.allocate_exact_size(available, Sense::hover());
    let rect = Rect::from_center_size(outer.center(), egui::vec2(car::W * scale, car::H * scale));
    let response = ui.interact(rect, ui.id().with("car_canvas"), Sense::click());
    board.canvas = Some(rect);
    let map = Map {
        origin: rect.min,
        scale,
    };
    let painter = ui.painter_at(rect.expand(8.0));
    let visuals = ui.visuals().clone();
    if let Some(drawing) = &board.drawing {
        painter.image(
            drawing.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            visuals
                .text_color()
                .gamma_multiply(if dark { 0.62 } else { 0.8 }),
        );
    }
    for (y, text) in [
        (12.0, t("FRÅN SIDAN", "FROM THE SIDE")),
        (316.0, t("UPPIFRÅN", "FROM ABOVE")),
    ] {
        painter.text(
            map.pt(16.0, y),
            egui::Align2::LEFT_TOP,
            text,
            FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }
    let hit = response
        .hover_pos()
        .or(response.interact_pointer_pos())
        .and_then(|p| board.car.hit(map.canvas(p)));

    if let Some(place) = board.place.as_mut() {
        let dept = snapshot.departments.iter().find(|d| d.id == place.dept);
        let color = dept
            .map(|d| theme::dept_color(d.color, dark))
            .unwrap_or(theme::ACCENT);
        let neutral = visuals.text_color();
        // Who else has the region, if anyone.
        let taken = |region: &Region| owner(snapshot, region.id).filter(|d| d.id != place.dept);
        for region in &board.car.regions {
            let hovered = hit.is_some_and(|h| h.id == region.id);
            let mine = place.regions.iter().any(|id| id == region.id);
            if mine {
                paint_region(
                    &painter,
                    &map,
                    region,
                    color,
                    if hovered { &HOVERED } else { &SELECTED },
                );
            } else if taken(region).is_some() {
                let look = Look {
                    fill: 12,
                    line: 1.0,
                    line_alpha: 60,
                    glow: false,
                };
                paint_region(&painter, &map, region, neutral, &look);
            } else {
                let look = Look {
                    fill: if hovered { 30 } else { 0 },
                    line: if hovered { 2.0 } else { 1.0 },
                    line_alpha: if hovered { 255 } else { 120 },
                    glow: hovered,
                };
                paint_region(
                    &painter,
                    &map,
                    region,
                    if hovered { color } else { neutral },
                    &look,
                );
            }
        }
        if let Some(region) = hit {
            let other = taken(region);
            let mine = place.regions.iter().any(|id| id == region.id);
            let response = if other.is_none() {
                response
                    .clone()
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
            } else {
                response.clone()
            };
            let clicked = response.clicked();
            response.on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(region.label()).strong());
                match other {
                    Some(d) => ui.weak(format!(
                        "{} {}",
                        t("Hör till", "Belongs to"),
                        truncate_chars(&one_line(&d.name), 40)
                    )),
                    None if mine => ui.weak(t("Klicka för att ta bort", "Click to remove")),
                    None => ui.weak(t("Klicka för att lägga till", "Click to add")),
                };
            });
            if clicked && other.is_none() {
                if mine {
                    place.regions.retain(|id| id != region.id);
                } else {
                    place.regions.push(region.id.to_string());
                }
            }
        }
        return;
    }

    let hovered_dept = hit.and_then(|region| owner(snapshot, region.id));
    for dept in &snapshot.departments {
        let (progress, _, _) = dept_progress(snapshot, dept.id);
        let color = theme::progress_color(progress, dark);
        let selected = board.selected == Some(dept.id);
        let hovered = hovered_dept.is_some_and(|d| d.id == dept.id);
        let look = match (hovered, selected) {
            (true, _) => &HOVERED,
            (false, true) => &SELECTED,
            _ => &IDLE,
        };
        // Only regions the department really has: one that is claimed twice
        // is drawn for the older department alone.
        for region in regions_of(&board.car, dept) {
            if owner(snapshot, region.id).is_some_and(|d| d.id == dept.id) {
                paint_region(&painter, &map, region, color, look);
            }
        }
    }
    // A part of the car that no department has yet: a faint outline on hover.
    if let (Some(region), None) = (hit, hovered_dept) {
        let look = Look {
            fill: 7,
            line: 1.0,
            line_alpha: 150,
            glow: false,
        };
        paint_region(&painter, &map, region, visuals.text_color(), &look);
    }
    // The selected department is named on the car, once per view.
    if let Some(dept) = snapshot
        .departments
        .iter()
        .find(|d| board.selected == Some(d.id))
    {
        let name = truncate_chars(&one_line(&dept.name), 28);
        for side_view in [true, false] {
            let largest = regions_of(&board.car, dept)
                .filter(|r| owner(snapshot, r.id).is_some_and(|d| d.id == dept.id))
                .filter_map(|r| r.largest(side_view))
                .max_by(|a, b| a.area.total_cmp(&b.area));
            if let Some(outline) = largest {
                tag(
                    &painter,
                    map.pt(outline.center.0, outline.center.1),
                    &name,
                    &visuals,
                );
            }
        }
    }

    if let Some(region) = hit {
        let response = match hovered_dept {
            Some(_) => response
                .clone()
                .on_hover_cursor(egui::CursorIcon::PointingHand),
            None => response.clone(),
        };
        response.on_hover_ui_at_pointer(|ui| match hovered_dept {
            Some(dept) => {
                let (progress, _, total) = dept_progress(snapshot, dept.id);
                ui.label(RichText::new(one_line(&dept.name)).strong());
                ui.weak(region.label());
                if total == 0 {
                    ui.weak(t("Inga delar än", "No parts yet"));
                } else {
                    ui.label(theme::percent_label(progress));
                    for status in PartStatus::ALL {
                        let count = parts_of(snapshot, dept.id)
                            .filter(|p| p.status == status)
                            .count();
                        if count > 0 {
                            ui.horizontal(|ui| {
                                theme::dot(ui, theme::part_status_color(status, dark));
                                ui.label(format!("{count} {}", status.label().to_lowercase()));
                            });
                        }
                    }
                }
            }
            None => {
                ui.label(RichText::new(region.label()).strong());
                ui.weak(t("Ingen avdelning än", "No department yet"));
            }
        });
    }
    // Only a click on a department's own shape does something: a click
    // between the parts or outside the car is ignored.
    if response.clicked() {
        if let Some(dept) = hovered_dept {
            out.push(BoardAction::Select(Some(dept.id)));
        }
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let placed = snapshot
            .departments
            .iter()
            .any(|d| regions_of(&board.car, d).next().is_some());
        if !placed {
            ui.weak(t(
                "Ingen avdelning är placerad på bilen än. Öppna en avdelning och klicka på \"Placera på bilen\".",
                "No department is placed on the car yet. Open a department and click \"Place on the car\".",
            ));
            return;
        }
        for (progress, label) in [
            (None, t("inga delar", "no parts")),
            (Some(0.0), "0 %"),
            (Some(0.5), "50 %"),
            (Some(1.0), "100 %"),
        ] {
            theme::dot(ui, theme::progress_color(progress, dark));
            ui.label(
                RichText::new(label)
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::sync::{Client, Submitted};
    use crate::testutil::TempDir;

    /// A snapshot with Aero on both wings (two parts) and Chassis, which is not placed.
    fn snapshot(dir: &TempDir) -> (Snapshot, Uuid, Uuid) {
        let mut client = Client::new(
            Store::open_in_memory().unwrap(),
            Uuid::new_v4(),
            "Anna".to_string(),
            Some(dir.path().to_path_buf()),
        )
        .unwrap();
        let mut add = |action: NewAction| match client.submit(None, &action) {
            Ok(Submitted::Applied { request_id, .. }) => request_id,
            other => panic!("unexpected: {other:?}"),
        };
        let aero = add(NewAction::DeptSet {
            name: "Aero".to_string(),
            color: 0,
            zones: Vec::new(),
            regions: vec![
                "front_wing".to_string(),
                "rear_wing".to_string(),
                "from_a_newer_app".to_string(),
            ],
        });
        let chassis = add(NewAction::DeptSet {
            name: "Chassis".to_string(),
            color: 1,
            zones: Vec::new(),
            regions: Vec::new(),
        });
        for (name, status) in [
            ("Front wing", PartStatus::Done),
            ("Rear wing", PartStatus::Design),
        ] {
            add(NewAction::PartSet {
                department: aero,
                name: name.to_string(),
                status,
                note: "Waiting for carbon fibre".to_string(),
            });
        }
        (client.snapshot(), aero, chassis)
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 800.0))),
            events,
            ..Default::default()
        }
    }

    /// Runs one frame of the whole overview with the given pointer events.
    fn frame(
        ctx: &egui::Context,
        board: &mut BoardUi,
        snapshot: &Snapshot,
        events: Vec<egui::Event>,
    ) -> Vec<BoardAction> {
        let mut out = Vec::new();
        let mut actions = Vec::new();
        let _ = ctx.run(input(events), |ctx| {
            egui::SidePanel::right("board_side").show(ctx, |ui| {
                side_ui(ui, board, snapshot, true, "", &mut out, &mut actions);
            });
            egui::CentralPanel::default().show(ctx, |ui| {
                canvas_ui(ui, board, snapshot, true, &mut out);
            });
        });
        out
    }

    /// Moves the pointer to a point of the drawing and clicks there.
    fn click(
        ctx: &egui::Context,
        board: &mut BoardUi,
        snapshot: &Snapshot,
        x: f32,
        y: f32,
    ) -> Vec<BoardAction> {
        let rect = board.canvas.expect("the car has been drawn");
        let scale = rect.width() / car::W;
        let pos = Pos2::new(rect.min.x + x * scale, rect.min.y + y * scale);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(ctx, board, snapshot, vec![egui::Event::PointerMoved(pos)]);
        frame(ctx, board, snapshot, vec![button(true)]);
        frame(ctx, board, snapshot, vec![button(false)])
    }

    #[test]
    fn only_a_click_on_a_departments_shape_selects_it() {
        let dir = TempDir::new();
        let (snapshot, aero, _) = snapshot(&dir);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let mut board = BoardUi::default();
        // Two frames: the side panel finds its width in the first one.
        assert!(frame(&ctx, &mut board, &snapshot, Vec::new()).is_empty());
        assert!(frame(&ctx, &mut board, &snapshot, Vec::new()).is_empty());
        // The drawing keeps its proportions.
        let rect = board.canvas.unwrap();
        assert!((rect.width() / rect.height() - car::W / car::H).abs() < 0.01);

        // The rear wing from the side, and the front wing from above.
        for (x, y) in [(1260.0, 130.0), (150.0, 420.0)] {
            let out = click(&ctx, &mut board, &snapshot, x, y);
            assert!(matches!(out[..], [BoardAction::Select(Some(id))] if id == aero));
        }
        // A part no department has, the gap between a wheel and the body, and
        // the empty corner: nothing happens, and the selection stays.
        board.selected = Some(aero);
        for (x, y) in [(345.0, 218.0), (460.0, 380.0), (20.0, 20.0)] {
            assert!(
                click(&ctx, &mut board, &snapshot, x, y).is_empty(),
                "({x}, {y})"
            );
        }
        assert_eq!(board.selected, Some(aero));
    }

    #[test]
    fn regions_are_chosen_by_clicking_and_taken_ones_are_left_alone() {
        let dir = TempDir::new();
        let (snapshot, aero, chassis) = snapshot(&dir);
        let ctx = egui::Context::default();
        let mut board = BoardUi {
            selected: Some(chassis),
            place: Some(Place {
                dept: chassis,
                regions: Vec::new(),
            }),
            ..BoardUi::default()
        };
        // Two frames: the banner above the car changes where the car ends up.
        frame(&ctx, &mut board, &snapshot, Vec::new());
        frame(&ctx, &mut board, &snapshot, Vec::new());
        let chosen = |board: &BoardUi| board.place.as_ref().unwrap().regions.clone();

        // The monocoque from above, then the nose from the side: both are added.
        click(&ctx, &mut board, &snapshot, 690.0, 540.0);
        click(&ctx, &mut board, &snapshot, 250.0, 215.0);
        assert_eq!(chosen(&board), ["monocoque", "nose"]);
        // The front wing belongs to Aero and cannot be taken; empty space does nothing.
        click(&ctx, &mut board, &snapshot, 150.0, 420.0);
        click(&ctx, &mut board, &snapshot, 20.0, 20.0);
        assert_eq!(chosen(&board), ["monocoque", "nose"]);
        // A second click, in the other view, removes the region again.
        click(&ctx, &mut board, &snapshot, 600.0, 120.0);
        assert_eq!(chosen(&board), ["nose"]);

        // Aero keeps the region it got from a newer app while its own are changed.
        board.place = Some(Place {
            dept: aero,
            regions: snapshot.departments[0].regions.clone(),
        });
        frame(&ctx, &mut board, &snapshot, Vec::new());
        click(&ctx, &mut board, &snapshot, 1260.0, 130.0);
        assert_eq!(chosen(&board), ["front_wing", "from_a_newer_app"]);
    }

    #[test]
    fn suggested_departments_use_every_region_once() {
        let car = Car::new();
        let mut used: Vec<&str> = SUGGESTED
            .iter()
            .flat_map(|(_, _, r)| r.iter().copied())
            .collect();
        assert!(SUGGESTED.len() <= usize::from(COLOR_COUNT));
        for id in &used {
            assert!(car.region(id).is_some(), "{id}");
        }
        used.sort_unstable();
        let mut all: Vec<&str> = car.regions.iter().map(|r| r.id).collect();
        all.sort_unstable();
        assert_eq!(used, all);
    }
}
