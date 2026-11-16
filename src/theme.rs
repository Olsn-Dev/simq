//! Look and feel: colours, spacing and a few small reusable widgets.
//!
//! The app follows the system's light or dark mode. Both themes are set up
//! once at start-up and egui switches between them by itself.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Stroke, TextStyle,
};

use crate::model::{PartStatus, Status};

/// The accent colour, also used in the app icon.
pub const ACCENT: Color32 = Color32::from_rgb(47, 108, 235);
const ACCENT_HOVER: Color32 = Color32::from_rgb(71, 128, 247);

/// App icon, embedded in the binary.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// Applies fonts, spacing and colours for both light and dark mode.
pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(21.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(11.0, 5.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.window_margin = Margin::same(16);
        style.spacing.menu_margin = Margin::same(8);
        style.spacing.combo_width = 120.0;
    });
    ctx.set_visuals_of(egui::Theme::Dark, dark());
    ctx.set_visuals_of(egui::Theme::Light, light());
}

fn round(visuals: &mut egui::Visuals) {
    let radius = CornerRadius::same(7);
    visuals.widgets.noninteractive.corner_radius = radius;
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;
    visuals.widgets.open.corner_radius = radius;
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(9);
}

fn dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    round(&mut v);
    v.panel_fill = Color32::from_rgb(22, 24, 29);
    v.window_fill = Color32::from_rgb(30, 33, 40);
    v.extreme_bg_color = Color32::from_rgb(14, 15, 19);
    v.faint_bg_color = Color32::from_rgb(27, 30, 36);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(52, 57, 68));
    v.hyperlink_color = Color32::from_rgb(120, 174, 255);
    v.selection.bg_fill = Color32::from_rgb(38, 70, 138);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(228, 237, 255));

    v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(208);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(46, 50, 60);
    v.widgets.inactive.fg_stroke.color = Color32::from_gray(222);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(43, 47, 57);
    v.widgets.inactive.bg_fill = Color32::from_rgb(43, 47, 57);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(56, 62, 76);
    v.widgets.hovered.bg_fill = Color32::from_rgb(56, 62, 76);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(88, 98, 120));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(66, 73, 90);
    v.widgets.active.bg_fill = Color32::from_rgb(66, 73, 90);
    v
}

fn light() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    round(&mut v);
    v.panel_fill = Color32::from_rgb(246, 247, 249);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(239, 241, 245);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(214, 218, 226));
    v.hyperlink_color = Color32::from_rgb(26, 95, 205);
    v.selection.bg_fill = Color32::from_rgb(207, 224, 255);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(18, 58, 150));

    v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(45);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(218, 221, 228);
    v.widgets.inactive.fg_stroke.color = Color32::from_gray(35);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(230, 233, 239);
    v.widgets.inactive.bg_fill = Color32::from_rgb(230, 233, 239);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(218, 223, 232);
    v.widgets.hovered.bg_fill = Color32::from_rgb(218, 223, 232);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(170, 180, 198));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(205, 212, 224);
    v.widgets.active.bg_fill = Color32::from_rgb(205, 212, 224);
    v
}

/// Loads the embedded app icon as a texture for use inside the window.
pub fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    Some(ctx.load_texture("simq-logo", image, egui::TextureOptions::LINEAR))
}

pub fn logo(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, size: f32) {
    if let Some(texture) = texture {
        ui.add(
            egui::Image::from_texture(egui::load::SizedTexture::from_handle(texture))
                .fit_to_exact_size(egui::vec2(size, size)),
        );
    }
}

pub fn status_color(status: Status, dark: bool) -> Color32 {
    match (status, dark) {
        (Status::Queued, true) => Color32::from_rgb(125, 178, 255),
        (Status::Queued, false) => Color32::from_rgb(25, 95, 200),
        (Status::Running, true) => Color32::from_rgb(255, 188, 88),
        (Status::Running, false) => Color32::from_rgb(176, 98, 0),
        (Status::Done, true) => Color32::from_rgb(124, 214, 136),
        (Status::Done, false) => Color32::from_rgb(20, 125, 50),
        (Status::Failed, true) => Color32::from_rgb(255, 125, 125),
        (Status::Failed, false) => Color32::from_rgb(195, 40, 40),
        (Status::Cancelled, true) => Color32::from_gray(165),
        (Status::Cancelled, false) => Color32::from_gray(100),
    }
}

pub fn priority_color(priority: u8, ui: &egui::Ui) -> Color32 {
    let dark = ui.visuals().dark_mode;
    match (priority, dark) {
        (1, true) => Color32::from_rgb(255, 150, 110),
        (1, false) => Color32::from_rgb(196, 70, 20),
        (3, _) => ui.visuals().weak_text_color(),
        _ => ui.visuals().text_color(),
    }
}

/// A small rounded label with a tinted background, e.g. a status.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> Response {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(12.5), color);
    let size = galley.size() + egui::vec2(18.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

pub fn status_pill(ui: &mut egui::Ui, status: Status) -> Response {
    let color = status_color(status, ui.visuals().dark_mode);
    pill(ui, status.label(), color)
}

/// The main action in a view: filled with the accent colour.
pub fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT)
}

/// Adds a primary button, slightly lighter while hovered.
pub fn add_primary(ui: &mut egui::Ui, enabled: bool, text: &str) -> Response {
    ui.scope(|ui| {
        ui.visuals_mut().widgets.hovered.weak_bg_fill = ACCENT_HOVER;
        ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::NONE;
        ui.visuals_mut().widgets.active.weak_bg_fill = ACCENT;
        ui.add_enabled(enabled, primary_button(text))
    })
    .inner
}

/// A small filled circle, used as a status light.
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A small upper-case heading above a group of fields.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(1.0);
}

/// A subtle card around a group of content.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(12))
}

/// A fixed-width, muted field label so that values line up in a column.
pub fn field_label(ui: &mut egui::Ui, label: &str) {
    let height = ui.text_style_height(&TextStyle::Body);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(92.0, height), egui::Sense::hover());
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        ui.visuals().weak_text_color(),
    );
}

/// The colours a department can have: (dark mode, light mode).
const DEPT_COLORS: [(Color32, Color32); 10] = [
    (
        Color32::from_rgb(110, 168, 255),
        Color32::from_rgb(30, 100, 210),
    ),
    (
        Color32::from_rgb(255, 160, 96),
        Color32::from_rgb(200, 96, 20),
    ),
    (
        Color32::from_rgb(120, 210, 140),
        Color32::from_rgb(24, 130, 60),
    ),
    (
        Color32::from_rgb(232, 130, 200),
        Color32::from_rgb(176, 50, 140),
    ),
    (
        Color32::from_rgb(190, 150, 255),
        Color32::from_rgb(110, 70, 200),
    ),
    (
        Color32::from_rgb(240, 205, 90),
        Color32::from_rgb(150, 115, 0),
    ),
    (
        Color32::from_rgb(90, 210, 215),
        Color32::from_rgb(0, 130, 140),
    ),
    (
        Color32::from_rgb(255, 125, 125),
        Color32::from_rgb(195, 45, 45),
    ),
    (
        Color32::from_rgb(170, 200, 110),
        Color32::from_rgb(95, 125, 20),
    ),
    (
        Color32::from_rgb(170, 178, 195),
        Color32::from_rgb(95, 105, 125),
    ),
];

pub fn dept_color(index: u8, dark: bool) -> Color32 {
    let (in_dark, in_light) = DEPT_COLORS[usize::from(index) % DEPT_COLORS.len()];
    if dark {
        in_dark
    } else {
        in_light
    }
}

pub fn part_status_color(status: PartStatus, dark: bool) -> Color32 {
    match status {
        PartStatus::NotStarted => status_color(Status::Cancelled, dark),
        PartStatus::Design => status_color(Status::Queued, dark),
        PartStatus::Simulation => dept_color(4, dark),
        PartStatus::Manufacturing => status_color(Status::Running, dark),
        PartStatus::Done => status_color(Status::Done, dark),
    }
}

/// Colour for how far something has come: grey when there is nothing to
/// measure, then from red over amber to green.
pub fn progress_color(progress: Option<f32>, dark: bool) -> Color32 {
    let Some(p) = progress else {
        return status_color(Status::Cancelled, dark);
    };
    let (low, mid, high) = (
        status_color(Status::Failed, dark),
        status_color(Status::Running, dark),
        status_color(Status::Done, dark),
    );
    let mix = |a: Color32, b: Color32, t: f32| {
        let ch = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
        Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
    };
    let p = p.clamp(0.0, 1.0);
    if p < 0.5 {
        mix(low, mid, p * 2.0)
    } else {
        mix(mid, high, (p - 0.5) * 2.0)
    }
}

/// A thin progress bar. `progress` is 0.0 to 1.0, or `None` for "nothing to measure".
pub fn progress_bar(ui: &mut egui::Ui, progress: Option<f32>, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
    let radius = CornerRadius::same(3);
    ui.painter()
        .rect_filled(rect, radius, ui.visuals().widgets.inactive.bg_fill);
    if let Some(p) = progress {
        let mut filled = rect;
        filled.set_width(rect.width() * p.clamp(0.0, 1.0));
        if filled.width() >= 1.0 {
            ui.painter().rect_filled(
                filled,
                radius,
                progress_color(progress, ui.visuals().dark_mode),
            );
        }
    }
    response
}

/// "60 %" or "–".
pub fn percent_label(progress: Option<f32>) -> String {
    match progress {
        Some(p) => format!("{} %", (p * 100.0).round() as u32),
        None => "–".to_string(),
    }
}
