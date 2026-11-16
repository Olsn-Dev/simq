//! The statistics view: how many requests there are, who asks for them and
//! how long they take.

use std::sync::Arc;

use chrono::Local;
use eframe::egui::{self, CornerRadius, RichText};

use super::App;
use crate::export::{self, Count};
use crate::i18n::t;
use crate::theme;

impl App {
    pub(super) fn stats_ui(&mut self, ctx: &egui::Context) {
        let snapshot = Arc::clone(&self.snapshot);
        let today = Local::now().format("%Y-%m-%d").to_string();
        let stats = export::stats(&snapshot.requests, &snapshot.departments, &today);
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(t("Statistik", "Statistics"))
                            .heading()
                            .strong(),
                    );
                    ui.weak(t(
                        "Räknat på alla ärenden som inte är borttagna.",
                        "Counted over all requests that are not deleted.",
                    ));
                    ui.add_space(8.0);
                    if stats.total == 0 {
                        ui.weak(t("Inga ärenden än.", "No requests yet."));
                        return;
                    }
                    let dark = ui.visuals().dark_mode;
                    ui.horizontal_wrapped(|ui| {
                        tile(ui, t("Ärenden", "Requests"), &stats.total.to_string(), None);
                        for (status, count) in &stats.by_status {
                            tile(
                                ui,
                                status.label(),
                                &count.to_string(),
                                Some(theme::status_color(*status, dark)),
                            );
                        }
                        tile(
                            ui,
                            t("Försenade", "Overdue"),
                            &stats.overdue.to_string(),
                            (stats.overdue > 0).then(|| ui.visuals().error_fg_color),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let time = |ms: Option<u64>| {
                            ms.map(export::duration_label)
                                .unwrap_or_else(|| "–".to_string())
                        };
                        tile(
                            ui,
                            t("Väntetid i snitt", "Average wait"),
                            &time(stats.avg_wait_ms),
                            None,
                        )
                        .on_hover_text(t(
                            "Från att ärendet skapades till att någon tog det",
                            "From the request being created until someone took it",
                        ));
                        tile(
                            ui,
                            t("Ledtid i snitt", "Average lead time"),
                            &time(stats.avg_lead_ms),
                            None,
                        )
                        .on_hover_text(t(
                            "Från att ärendet skapades till att det blev klart",
                            "From the request being created until it was done",
                        ));
                    });

                    bars(
                        ui,
                        t("Per avdelning", "By department"),
                        &stats.by_department,
                    );
                    bars(
                        ui,
                        t("Per simuleringstyp", "By simulation type"),
                        &stats.by_type,
                    );
                    bars(ui, t("Per utförare", "By assignee"), &stats.by_assignee);
                    ui.add_space(12.0);
                });
        });
    }
}

/// A small card with a number and what it counts.
fn tile(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    color: Option<egui::Color32>,
) -> egui::Response {
    theme::card(ui)
        .show(ui, |ui| {
            ui.set_min_width(96.0);
            ui.vertical(|ui| {
                let text = RichText::new(value).size(22.0).strong();
                ui.label(match color {
                    Some(color) => text.color(color),
                    None => text,
                });
                ui.label(
                    RichText::new(label)
                        .size(12.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        })
        .response
}

/// A list of horizontal bars. The darker part is what is still queued or in progress.
fn bars(ui: &mut egui::Ui, title: &str, counts: &[Count]) {
    theme::section(ui, title);
    ui.add_space(2.0);
    if counts.is_empty() {
        ui.weak("–");
        return;
    }
    let largest = counts.iter().map(|c| c.total).max().unwrap_or(1).max(1);
    let width = (ui.available_width() - 260.0).clamp(120.0, 520.0);
    for count in counts.iter().take(12) {
        ui.horizontal(|ui| {
            let (label_rect, _) =
                ui.allocate_exact_size(egui::vec2(170.0, 18.0), egui::Sense::hover());
            ui.painter().with_clip_rect(label_rect).text(
                label_rect.left_center(),
                egui::Align2::LEFT_CENTER,
                &count.label,
                egui::FontId::proportional(13.5),
                ui.visuals().text_color(),
            );
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
            let radius = CornerRadius::same(4);
            ui.painter()
                .rect_filled(rect, radius, ui.visuals().widgets.inactive.bg_fill);
            let mut total = rect;
            total.set_width(rect.width() * count.total as f32 / largest as f32);
            ui.painter()
                .rect_filled(total, radius, theme::ACCENT.gamma_multiply(0.45));
            let mut active = rect;
            active.set_width(rect.width() * count.active as f32 / largest as f32);
            if count.active > 0 {
                ui.painter().rect_filled(active, radius, theme::ACCENT);
            }
            response.on_hover_text(t(
                "Den starkare delen är köade och pågående ärenden",
                "The stronger part is queued and in-progress requests",
            ));
            ui.label(count.total.to_string());
            if count.active > 0 {
                ui.weak(format!("({} {})", count.active, t("öppna", "open")));
            }
        });
    }
    if counts.len() > 12 {
        ui.weak("…");
    }
}
