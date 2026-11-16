//! The overview: a drawing of a Formula Student car where every department
//! marks its own zones. A department works like a folder of parts, and the
//! colour of its zones shows how far the parts have come.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind,
};
use uuid::Uuid;

use super::{App, Confirm, ConfirmWhat, Pending, UiAction};
use crate::i18n::{is_en, t};
use crate::model::{
    clean_text, one_line, progress, truncate_chars, Department, Part, PartStatus, Zone, CANVAS_H,
    CANVAS_W, COLOR_COUNT, MAX_DEPT_NAME, MAX_PART_NAME, MAX_TEXT, MAX_ZONES,
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
    zone_edit: Option<ZoneEdit>,
    /// Where the car was drawn in the last frame.
    canvas: Option<Rect>,
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

/// The zones of a department while the user is placing them on the car.
struct ZoneEdit {
    dept: Uuid,
    zones: Vec<Zone>,
    /// Where the current drag started, in the drawing's units.
    drag_from: Option<(f32, f32)>,
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
    EditZones(Uuid),
    SaveZones,
    CancelZones,
    EditText(Uuid, EditWhat, String),
    SaveText,
    CancelText,
}

/// Suggested departments with zones that fit the drawing: (Swedish, English, zones).
const SUGGESTED: [(&str, &str, &[[u16; 4]]); 6] = [
    (
        "Aero",
        "Aero",
        &[
            [75, 35, 125, 115],
            [795, 195, 170, 50],
            [75, 325, 125, 230],
            [805, 315, 145, 250],
        ],
    ),
    (
        "Chassi",
        "Chassis",
        &[[470, 92, 190, 72], [405, 400, 350, 80]],
    ),
    (
        "Hjulupphängning",
        "Suspension",
        &[
            [200, 155, 100, 100],
            [670, 155, 100, 100],
            [200, 322, 100, 56],
            [200, 502, 100, 56],
            [670, 330, 100, 52],
            [670, 498, 100, 52],
        ],
    ),
    (
        "Drivlina",
        "Powertrain",
        &[[305, 152, 85, 72], [205, 405, 190, 70]],
    ),
    (
        "Elektronik",
        "Electronics",
        &[[400, 172, 95, 50], [400, 354, 200, 44]],
    ),
    (
        "Batteri",
        "Battery",
        &[[500, 172, 95, 50], [400, 482, 200, 44]],
    ),
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

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_max(self.pt(x, y), self.pt(x + w, y + h))
    }

    fn zone(&self, z: &Zone) -> Rect {
        self.rect(
            f32::from(z.x),
            f32::from(z.y),
            f32::from(z.w),
            f32::from(z.h),
        )
    }

    /// A screen position in the drawing's units, kept inside the drawing.
    fn canvas(&self, p: Pos2) -> (f32, f32) {
        (
            ((p.x - self.origin.x) / self.scale).clamp(0.0, f32::from(CANVAS_W)),
            ((p.y - self.origin.y) / self.scale).clamp(0.0, f32::from(CANVAS_H)),
        )
    }
}

fn zone_between(a: (f32, f32), b: (f32, f32)) -> Zone {
    let (x0, x1) = (a.0.min(b.0).round(), a.0.max(b.0).round());
    let (y0, y1) = (a.1.min(b.1).round(), a.1.max(b.1).round());
    Zone {
        x: x0 as u16,
        y: y0 as u16,
        w: (x1 - x0) as u16,
        h: (y1 - y0) as u16,
    }
}

fn contains(zone: &Zone, at: (f32, f32)) -> bool {
    at.0 >= f32::from(zone.x)
        && at.0 <= f32::from(zone.x + zone.w)
        && at.1 >= f32::from(zone.y)
        && at.1 <= f32::from(zone.y + zone.h)
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
        if self.board.zone_edit.as_ref().is_some_and(|e| !live(e.dept)) {
            self.board.zone_edit = None;
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
                    },
                    Vec::new(),
                    Pending::Other,
                );
                self.board.new_dept.clear();
                self.board.selected = Some(id);
            }
            BoardAction::AddSuggested => {
                let items = SUGGESTED
                    .iter()
                    .enumerate()
                    .map(|(i, (sv, en, zones))| {
                        (
                            Uuid::new_v4(),
                            NewAction::DeptSet {
                                name: t(sv, en).to_string(),
                                color: i as u8,
                                zones: zones
                                    .iter()
                                    .map(|[x, y, w, h]| Zone {
                                        x: *x,
                                        y: *y,
                                        w: *w,
                                        h: *h,
                                    })
                                    .collect(),
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
            BoardAction::EditZones(id) => {
                if let Some(d) = dept(id) {
                    self.board.zone_edit = Some(ZoneEdit {
                        dept: id,
                        zones: d.zones.clone(),
                        drag_from: None,
                    });
                }
            }
            BoardAction::SaveZones => {
                if let Some(edit) = self.board.zone_edit.take() {
                    if let Some(d) = dept(edit.dept) {
                        if d.zones != edit.zones {
                            let changed = Department {
                                zones: edit.zones,
                                ..d.clone()
                            };
                            self.submit(Some(d.id), set_dept(&changed), Vec::new(), Pending::Other);
                        }
                    }
                }
            }
            BoardAction::CancelZones => self.board.zone_edit = None,
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
                "Skapar Aero, Chassi, Hjulupphängning, Drivlina, Elektronik och Batteri med färdiga zoner. Allt kan ändras efteråt.",
                "Creates Aero, Chassis, Suspension, Powertrain, Electronics and Battery with ready-made zones. Everything can be changed afterwards.",
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
                if dept.zones.is_empty() {
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
    ui.add_space(6.0);
    ui.add_enabled_ui(can_write, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(t("Placera på bilen", "Place on the car"))
                .on_hover_text(t(
                    "Markera var på ritningen avdelningen hör hemma",
                    "Mark where on the drawing the department belongs",
                ))
                .on_disabled_hover_text(why)
                .clicked()
            {
                out.push(BoardAction::EditZones(dept.id));
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

fn canvas_ui(
    ui: &mut egui::Ui,
    board: &mut BoardUi,
    snapshot: &Snapshot,
    can_write: bool,
    out: &mut Vec<BoardAction>,
) {
    let dark = ui.visuals().dark_mode;
    if let Some(edit) = &board.zone_edit {
        let name = snapshot
            .departments
            .iter()
            .find(|d| d.id == edit.dept)
            .map(|d| truncate_chars(&one_line(&d.name), 40))
            .unwrap_or_default();
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(name).strong());
                ui.label(t(
                    "Dra på bilen för att markera en zon. Klicka på × för att ta bort en.",
                    "Drag on the car to mark a zone. Click × to remove one.",
                ));
                if theme::add_primary(ui, can_write, t("Spara", "Save")).clicked() {
                    out.push(BoardAction::SaveZones);
                }
                if ui.button(t("Avbryt", "Cancel")).clicked() {
                    out.push(BoardAction::CancelZones);
                }
                if edit.zones.len() >= MAX_ZONES {
                    ui.weak(t(
                        "Största antalet zoner är nått.",
                        "The largest number of zones is reached.",
                    ));
                }
            });
        });
        ui.add_space(4.0);
    }

    let available = ui.available_size();
    let scale = (available.x / f32::from(CANVAS_W))
        .min(available.y / f32::from(CANVAS_H))
        .max(0.2);
    let (outer, _) = ui.allocate_exact_size(available, Sense::hover());
    let rect = Rect::from_center_size(
        outer.center(),
        egui::vec2(f32::from(CANVAS_W) * scale, f32::from(CANVAS_H) * scale),
    );
    let response = ui.interact(rect, ui.id().with("car_canvas"), Sense::click_and_drag());
    board.canvas = Some(rect);
    let map = Map {
        origin: rect.min,
        scale,
    };
    let painter = ui.painter_at(rect.expand(2.0));
    draw_car(&painter, &map, ui.visuals());
    let pointer = response
        .hover_pos()
        .or(response.interact_pointer_pos())
        .map(|p| map.canvas(p));
    let font = FontId::proportional((13.0 * scale.clamp(0.75, 1.3)).round());
    let text_color = ui.visuals().strong_text_color();

    if let Some(edit) = board.zone_edit.as_mut() {
        // The other departments stay visible, but faint.
        let faint = with_alpha(ui.visuals().text_color(), 22);
        for dept in snapshot.departments.iter().filter(|d| d.id != edit.dept) {
            for zone in &dept.zones {
                painter.rect_filled(map.zone(zone), CornerRadius::same(6), faint);
            }
        }
        let color = snapshot
            .departments
            .iter()
            .find(|d| d.id == edit.dept)
            .map(|d| theme::dept_color(d.color, dark))
            .unwrap_or(theme::ACCENT);
        let handle = |zone: &Zone| {
            let r = map.zone(zone);
            Pos2::new(r.right() - 12.0, r.top() + 12.0)
        };
        for zone in &edit.zones {
            painter.rect(
                map.zone(zone),
                CornerRadius::same(6),
                with_alpha(color, 70),
                Stroke::new(2.0_f32, color),
                StrokeKind::Inside,
            );
            painter.circle_filled(handle(zone), 9.0, ui.visuals().window_fill);
            painter.circle_stroke(handle(zone), 9.0, Stroke::new(1.0_f32, color));
            painter.text(
                handle(zone),
                egui::Align2::CENTER_CENTER,
                "×",
                FontId::proportional(14.0),
                text_color,
            );
        }
        if response.drag_started() {
            // A drag is only noticed after the pointer has moved a little, so
            // the zone starts where the button went down, not where it is now.
            edit.drag_from = ui
                .input(|i| i.pointer.press_origin())
                .map(|p| map.canvas(p))
                .or(pointer);
        }
        if let (Some(from), Some(to)) = (edit.drag_from, pointer) {
            let zone = zone_between(from, to);
            painter.rect(
                map.zone(&zone),
                CornerRadius::same(6),
                with_alpha(color, 40),
                Stroke::new(1.5_f32, color),
                StrokeKind::Inside,
            );
            if response.drag_stopped() {
                if zone.valid() && edit.zones.len() < MAX_ZONES {
                    edit.zones.push(zone);
                }
                edit.drag_from = None;
            }
        } else if response.drag_stopped() {
            edit.drag_from = None;
        }
        if response.clicked() {
            if let Some(at) = response.interact_pointer_pos() {
                if let Some(index) = edit
                    .zones
                    .iter()
                    .rposition(|zone| handle(zone).distance(at) <= 11.0)
                {
                    edit.zones.remove(index);
                }
            }
        }
        return;
    }

    // The zone under the pointer: the smallest one wins when zones overlap.
    let hit = pointer.and_then(|at| {
        snapshot
            .departments
            .iter()
            .flat_map(|d| d.zones.iter().map(move |z| (d, z)))
            .filter(|(_, z)| contains(z, at))
            .min_by_key(|(_, z)| u32::from(z.w) * u32::from(z.h))
            .map(|(d, _)| d)
    });
    for dept in &snapshot.departments {
        let (progress, _, _) = dept_progress(snapshot, dept.id);
        let color = theme::progress_color(progress, dark);
        let active = board.selected == Some(dept.id) || hit.is_some_and(|d| d.id == dept.id);
        for zone in &dept.zones {
            let r = map.zone(zone);
            painter.rect(
                r,
                CornerRadius::same(6),
                with_alpha(color, if active { 90 } else { 44 }),
                Stroke::new(if active { 2.5_f32 } else { 1.5_f32 }, color),
                StrokeKind::Inside,
            );
            // The name, shortened until it fits, and the progress below it if there is room.
            let name = one_line(&dept.name);
            let fitted = (1..=name.chars().count()).rev().find_map(|n| {
                let text = truncate_chars(&name, n);
                let galley = painter.layout_no_wrap(text, font.clone(), text_color);
                (galley.size().x <= r.width() - 8.0).then_some(galley)
            });
            if let Some(galley) = fitted.filter(|g| g.size().y <= r.height() - 4.0) {
                let two_lines = r.height() >= galley.size().y * 2.0 + 8.0;
                let mut top = r.center().y - galley.size().y / 2.0;
                if two_lines {
                    top -= galley.size().y / 2.0;
                    painter.text(
                        Pos2::new(r.center().x, top + galley.size().y),
                        egui::Align2::CENTER_TOP,
                        theme::percent_label(progress),
                        font.clone(),
                        text_color,
                    );
                }
                painter.galley(
                    Pos2::new(r.center().x - galley.size().x / 2.0, top),
                    galley,
                    text_color,
                );
            }
        }
    }

    // Legend in the lower left corner.
    let mut x = 24.0;
    for (progress, label) in [
        (None, t("inga delar", "no parts")),
        (Some(0.0), "0 %"),
        (Some(0.5), "50 %"),
        (Some(1.0), "100 %"),
    ] {
        let at = map.pt(x, f32::from(CANVAS_H) - 14.0);
        painter.circle_filled(at, 4.5, theme::progress_color(progress, dark));
        let end = painter.text(
            at + egui::vec2(9.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.5),
            ui.visuals().weak_text_color(),
        );
        x = (end.right() - map.origin.x) / map.scale + 16.0 / map.scale;
    }

    if let Some(dept) = hit {
        let (progress, _, total) = dept_progress(snapshot, dept.id);
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(one_line(&dept.name)).strong());
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
            });
    } else if snapshot.departments.iter().all(|d| d.zones.is_empty()) {
        painter.text(
            map.pt(f32::from(CANVAS_W) / 2.0, 284.0),
            egui::Align2::CENTER_CENTER,
            t(
                "Inga zoner än. Välj en avdelning och klicka på \"Placera på bilen\".",
                "No zones yet. Choose a department and click \"Place on the car\".",
            ),
            FontId::proportional(13.0),
            ui.visuals().weak_text_color(),
        );
    }
    if response.clicked() {
        out.push(BoardAction::Select(hit.map(|d| d.id)));
    }
}

/// A stylised Formula Student car: from the side at the top, from above below.
fn draw_car(painter: &egui::Painter, map: &Map, visuals: &egui::Visuals) {
    let ink = visuals.text_color().gamma_multiply(0.7);
    let line = Stroke::new((1.6 * map.scale).clamp(1.0, 2.2), ink);
    let thin = Stroke::new(1.0_f32, visuals.text_color().gamma_multiply(0.3));
    let body = visuals.widgets.inactive.bg_fill;
    let tyre = visuals.widgets.active.bg_fill;
    let pts = |points: &[(f32, f32)]| -> Vec<Pos2> {
        points.iter().map(|(x, y)| map.pt(*x, *y)).collect()
    };
    let polygon = |points: &[(f32, f32)], fill: Color32| {
        painter.add(egui::Shape::convex_polygon(pts(points), fill, line));
    };
    let path = |points: &[(f32, f32)], stroke: Stroke| {
        painter.add(egui::Shape::line(pts(points), stroke));
    };
    let seg = |a: (f32, f32), b: (f32, f32), stroke: Stroke| {
        painter.line_segment([map.pt(a.0, a.1), map.pt(b.0, b.1)], stroke);
    };
    let boxed = |x: f32, y: f32, w: f32, h: f32, radius: u8, fill: Color32, stroke: Stroke| {
        painter.rect(
            map.rect(x, y, w, h),
            CornerRadius::same(radius),
            fill,
            stroke,
            StrokeKind::Inside,
        );
    };
    let label = |x: f32, y: f32, text: &str| {
        painter.text(
            map.pt(x, y),
            egui::Align2::LEFT_TOP,
            text,
            FontId::proportional(11.5),
            visuals.weak_text_color(),
        );
    };

    // ---- From the side (the nose points right) ----
    label(24.0, 14.0, t("FRÅN SIDAN", "FROM THE SIDE"));
    seg((40.0, 251.0), (960.0, 251.0), thin);
    // Rear wing with its mount.
    seg((165.0, 138.0), (215.0, 172.0), line);
    boxed(85.0, 45.0, 105.0, 95.0, 4, body, line);
    seg((97.0, 72.0), (178.0, 60.0), thin);
    seg((97.0, 98.0), (178.0, 88.0), thin);
    seg((97.0, 122.0), (178.0, 114.0), thin);
    // Diffuser and undertray.
    polygon(
        &[
            (180.0, 205.0),
            (300.0, 225.0),
            (300.0, 237.0),
            (180.0, 222.0),
        ],
        body,
    );
    polygon(
        &[
            (300.0, 225.0),
            (660.0, 225.0),
            (650.0, 237.0),
            (300.0, 237.0),
        ],
        body,
    );
    // Body.
    polygon(
        &[
            (180.0, 225.0),
            (180.0, 172.0),
            (385.0, 150.0),
            (560.0, 150.0),
            (760.0, 172.0),
            (885.0, 198.0),
            (888.0, 212.0),
            (800.0, 225.0),
        ],
        body,
    );
    // Main hoop with brace, front hoop, steering wheel and helmet.
    path(&[(395.0, 150.0), (428.0, 60.0), (458.0, 150.0)], line);
    seg((428.0, 60.0), (300.0, 159.0), thin);
    path(&[(575.0, 150.0), (592.0, 118.0), (612.0, 153.0)], line);
    seg((556.0, 140.0), (572.0, 116.0), line);
    painter.circle(map.pt(478.0, 118.0), 22.0 * map.scale, body, line);
    // Sidepod.
    boxed(400.0, 172.0, 190.0, 48.0, 10, Color32::TRANSPARENT, thin);
    // Suspension and wheels.
    for (cx, arms) in [
        (250.0, [(190.0, 182.0), (322.0, 176.0)]),
        (720.0, [(650.0, 178.0), (790.0, 184.0)]),
    ] {
        for arm in arms {
            seg((cx, 205.0), arm, thin);
        }
        painter.circle(map.pt(cx, 205.0), 45.0 * map.scale, tyre, line);
        painter.circle(map.pt(cx, 205.0), 22.0 * map.scale, body, thin);
    }
    // Front wing.
    polygon(
        &[
            (805.0, 213.0),
            (955.0, 206.0),
            (958.0, 238.0),
            (805.0, 238.0),
        ],
        body,
    );
    seg((820.0, 222.0), (945.0, 216.0), thin);
    seg((905.0, 209.0), (905.0, 238.0), thin);

    // ---- From above ----
    label(24.0, 296.0, t("UPPIFRÅN", "FROM ABOVE"));
    // Wings.
    boxed(85.0, 335.0, 105.0, 210.0, 4, body, line);
    seg((120.0, 337.0), (120.0, 543.0), thin);
    seg((155.0, 337.0), (155.0, 543.0), thin);
    boxed(815.0, 325.0, 125.0, 230.0, 4, body, line);
    seg((858.0, 327.0), (858.0, 553.0), thin);
    seg((900.0, 327.0), (900.0, 553.0), thin);
    // Suspension arms.
    for (wheel, inner) in [
        ((230.0, 372.0), (212.0, 410.0)),
        ((275.0, 372.0), (300.0, 402.0)),
        ((700.0, 376.0), (668.0, 410.0)),
        ((745.0, 376.0), (772.0, 418.0)),
    ] {
        seg(wheel, inner, thin);
        seg((wheel.0, 880.0 - wheel.1), (inner.0, 880.0 - inner.1), thin);
    }
    // Sidepods.
    boxed(405.0, 358.0, 190.0, 38.0, 8, body, line);
    boxed(405.0, 484.0, 190.0, 38.0, 8, body, line);
    // Body.
    polygon(
        &[
            (180.0, 408.0),
            (400.0, 395.0),
            (560.0, 395.0),
            (760.0, 417.0),
            (890.0, 433.0),
            (890.0, 447.0),
            (760.0, 463.0),
            (560.0, 485.0),
            (400.0, 485.0),
            (180.0, 472.0),
        ],
        body,
    );
    // Cockpit and helmet.
    boxed(425.0, 416.0, 150.0, 48.0, 14, Color32::TRANSPARENT, thin);
    painter.circle(map.pt(478.0, 440.0), 19.0 * map.scale, body, line);
    seg((585.0, 426.0), (585.0, 454.0), line);
    // Wheels.
    for (x, y, w, h) in [
        (205.0, 328.0, 90.0, 44.0),
        (205.0, 508.0, 90.0, 44.0),
        (675.0, 336.0, 90.0, 40.0),
        (675.0, 504.0, 90.0, 40.0),
    ] {
        boxed(x, y, w, h, 7, tyre, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::sync::{Client, Submitted};
    use crate::testutil::TempDir;

    /// A snapshot with two departments: one with a zone and two parts, one without zones.
    fn snapshot(dir: &TempDir) -> (Snapshot, Uuid) {
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
            zones: vec![Zone {
                x: 100,
                y: 100,
                w: 200,
                h: 100,
            }],
        });
        add(NewAction::DeptSet {
            name: "Chassi".to_string(),
            color: 1,
            zones: Vec::new(),
        });
        for (name, status) in [
            ("Framvinge", PartStatus::Done),
            ("Bakvinge", PartStatus::Design),
        ] {
            add(NewAction::PartSet {
                department: aero,
                name: name.to_string(),
                status,
                note: "Väntar på kolfiber".to_string(),
            });
        }
        (client.snapshot(), aero)
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 800.0))),
            events,
            ..Default::default()
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
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

    /// The screen position of a point given in the drawing's units.
    fn at(board: &BoardUi, x: f32, y: f32) -> Pos2 {
        let rect = board.canvas.expect("the car has been drawn");
        let scale = rect.width() / f32::from(CANVAS_W);
        Pos2::new(rect.min.x + x * scale, rect.min.y + y * scale)
    }

    #[test]
    fn clicking_a_zone_selects_its_department() {
        let dir = TempDir::new();
        let (snapshot, aero) = snapshot(&dir);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let mut board = BoardUi::default();
        assert!(frame(&ctx, &mut board, &snapshot, Vec::new()).is_empty());
        // The drawing keeps its proportions.
        let rect = board.canvas.unwrap();
        assert!((rect.width() / rect.height() - 1000.0 / 600.0).abs() < 0.01);

        let inside = at(&board, 200.0, 150.0);
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(inside)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(inside, true)]);
        let out = frame(&ctx, &mut board, &snapshot, vec![button(inside, false)]);
        assert!(matches!(out[..], [BoardAction::Select(Some(id))] if id == aero));

        // The department view with its parts renders, and a click beside the zones goes back.
        board.selected = Some(aero);
        frame(&ctx, &mut board, &snapshot, Vec::new());
        let outside = at(&board, 600.0, 280.0);
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(outside)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(outside, true)]);
        let out = frame(&ctx, &mut board, &snapshot, vec![button(outside, false)]);
        assert!(matches!(out[..], [BoardAction::Select(None)]));
    }

    #[test]
    fn zones_are_drawn_by_dragging_and_removed_with_the_cross() {
        let dir = TempDir::new();
        let (snapshot, aero) = snapshot(&dir);
        let ctx = egui::Context::default();
        let mut board = BoardUi {
            selected: Some(aero),
            zone_edit: Some(ZoneEdit {
                dept: aero,
                zones: snapshot.departments[0].zones.clone(),
                drag_from: None,
            }),
            ..BoardUi::default()
        };
        // Two frames: the banner above the car changes where the car ends up.
        frame(&ctx, &mut board, &snapshot, Vec::new());
        frame(&ctx, &mut board, &snapshot, Vec::new());

        let (from, to) = (at(&board, 500.0, 320.0), at(&board, 700.0, 420.0));
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(from)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(from, true)]);
        let middle = from + (to - from) / 2.0;
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(middle)],
        );
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(to)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(to, false)]);
        let zones = board.zone_edit.as_ref().unwrap().zones.clone();
        assert_eq!(zones.len(), 2, "the drag added a zone");
        let new = zones[1];
        assert!(new.valid());
        assert!((i32::from(new.x) - 500).abs() <= 2 && (i32::from(new.y) - 320).abs() <= 2);
        assert!((i32::from(new.w) - 200).abs() <= 3 && (i32::from(new.h) - 100).abs() <= 3);

        // A drag that is too small adds nothing.
        let (from, to) = (at(&board, 800.0, 100.0), at(&board, 806.0, 160.0));
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(from)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(from, true)]);
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(to)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(to, false)]);
        assert_eq!(board.zone_edit.as_ref().unwrap().zones.len(), 2);

        // The cross in the corner of the first zone removes it.
        let corner = at(&board, 300.0, 100.0) + egui::vec2(-12.0, 12.0);
        frame(
            &ctx,
            &mut board,
            &snapshot,
            vec![egui::Event::PointerMoved(corner)],
        );
        frame(&ctx, &mut board, &snapshot, vec![button(corner, true)]);
        frame(&ctx, &mut board, &snapshot, vec![button(corner, false)]);
        assert_eq!(board.zone_edit.as_ref().unwrap().zones, vec![new]);
    }

    #[test]
    fn suggested_zones_fit_the_drawing() {
        assert!(SUGGESTED.len() <= usize::from(COLOR_COUNT));
        for (name, _, zones) in SUGGESTED {
            assert!(zones.len() <= MAX_ZONES);
            for [x, y, w, h] in zones {
                let zone = Zone {
                    x: *x,
                    y: *y,
                    w: *w,
                    h: *h,
                };
                assert!(zone.valid(), "{name}: {zone:?}");
            }
        }
    }
}
