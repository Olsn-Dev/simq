//! Look and feel: colours, spacing and a few small reusable widgets.
//!
//! The app follows the system's light or dark mode. Both themes are set up
//! once at start-up and egui switches between them by itself.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Stroke, TextStyle,
};

use crate::model::Status;

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
