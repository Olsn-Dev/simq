//! Look and feel: a racing theme with carbon greys, racing red, slanted
//! primary buttons and the Titillium Web typeface.
//!
//! Both a light and a dark theme are set up once at start-up, and egui
//! switches between them.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Response, RichText, Sense, Stroke,
    TextStyle,
};

use crate::model::{PartStatus, Status};

/// Racing red: the accent colour, also used in the app icon.
pub const ACCENT: Color32 = Color32::from_rgb(225, 30, 38);
const ACCENT_HOVER: Color32 = Color32::from_rgb(242, 62, 64);
const ACCENT_PRESSED: Color32 = Color32::from_rgb(186, 20, 28);

/// App icon, embedded in the binary.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/TitilliumWeb-Regular.ttf");
const FONT_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/TitilliumWeb-SemiBold.ttf");

/// The heavier cut of the typeface, for headings, buttons and labels.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, semibold())
}

/// Applies fonts, spacing and colours for both light and dark mode.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "titillium".to_owned(),
        Arc::new(egui::FontData::from_static(FONT_REGULAR)),
    );
    fonts.font_data.insert(
        "titillium-semibold".to_owned(),
        Arc::new(egui::FontData::from_static(FONT_SEMIBOLD)),
    );
    // egui's own fonts stay as fallbacks, for symbols the typeface lacks.
    let mut fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, "titillium".to_owned());
    }
    fallbacks.insert(0, "titillium-semibold".to_owned());
    fonts.families.insert(semibold(), fallbacks);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.5));
        style
            .text_styles
            .insert(TextStyle::Button, semibold_font(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, semibold_font(22.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 3.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.window_margin = Margin::same(16);
        style.spacing.menu_margin = Margin::same(8);
        style.spacing.combo_width = 120.0;
    });
    ctx.set_visuals_of(egui::Theme::Dark, dark());
    ctx.set_visuals_of(egui::Theme::Light, light());
}

fn round(visuals: &mut egui::Visuals) {
    // Tight corners: more pit wall than phone app.
    let radius = CornerRadius::same(4);
    visuals.widgets.noninteractive.corner_radius = radius;
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;
    visuals.widgets.open.corner_radius = radius;
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(6);
}

fn dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    round(&mut v);
    v.panel_fill = Color32::from_rgb(17, 18, 22);
    v.window_fill = Color32::from_rgb(25, 26, 32);
    v.extreme_bg_color = Color32::from_rgb(10, 11, 14);
    v.faint_bg_color = Color32::from_rgb(23, 24, 30);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(52, 54, 64));
    v.hyperlink_color = Color32::from_rgb(120, 174, 255);
    v.selection.bg_fill = Color32::from_rgb(112, 22, 28);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(255, 236, 236));

    v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(210);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(44, 46, 55);
    v.widgets.inactive.fg_stroke.color = Color32::from_gray(226);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(38, 40, 48);
    v.widgets.inactive.bg_fill = Color32::from_rgb(38, 40, 48);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(58, 60, 72));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 54, 65);
    v.widgets.hovered.bg_fill = Color32::from_rgb(52, 54, 65);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(225, 30, 38));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(64, 66, 80);
    v.widgets.active.bg_fill = Color32::from_rgb(64, 66, 80);
    v
}

fn light() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    round(&mut v);
    v.panel_fill = Color32::from_rgb(245, 245, 247);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(237, 238, 242);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(212, 214, 222));
    v.hyperlink_color = Color32::from_rgb(26, 95, 205);
    v.selection.bg_fill = Color32::from_rgb(255, 214, 212);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(120, 12, 18));

    v.widgets.noninteractive.fg_stroke.color = Color32::from_gray(42);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(216, 218, 226);
    v.widgets.inactive.fg_stroke.color = Color32::from_gray(32);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(229, 231, 237);
    v.widgets.inactive.bg_fill = Color32::from_rgb(229, 231, 237);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(206, 209, 218));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(218, 221, 230);
    v.widgets.hovered.bg_fill = Color32::from_rgb(218, 221, 230);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(225, 30, 38));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(204, 208, 220);
    v.widgets.active.bg_fill = Color32::from_rgb(204, 208, 220);
    v
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Plus,
    Close,
    More,
    Back,
    Search,
    Download,
    Export,
    Sun,
    Moon,
    Gear,
    Sync,
    Comment,
}

impl Icon {
    const ALL: [(Icon, &'static [u8]); 12] = [
        (Icon::Plus, include_bytes!("../assets/icons/plus.png")),
        (Icon::Close, include_bytes!("../assets/icons/close.png")),
        (Icon::More, include_bytes!("../assets/icons/more.png")),
        (Icon::Back, include_bytes!("../assets/icons/back.png")),
        (Icon::Search, include_bytes!("../assets/icons/search.png")),
        (
            Icon::Download,
            include_bytes!("../assets/icons/download.png"),
        ),
        (Icon::Export, include_bytes!("../assets/icons/export.png")),
        (Icon::Sun, include_bytes!("../assets/icons/sun.png")),
        (Icon::Moon, include_bytes!("../assets/icons/moon.png")),
        (Icon::Gear, include_bytes!("../assets/icons/gear.png")),
        (Icon::Sync, include_bytes!("../assets/icons/sync.png")),
        (Icon::Comment, include_bytes!("../assets/icons/comment.png")),
    ];
}

/// The small icons on buttons: white drawings, tinted with the text colour.
#[derive(Clone, Default)]
pub struct Icons {
    textures: Vec<(Icon, egui::TextureHandle)>,
}

impl Icons {
    pub fn load(ctx: &egui::Context) -> Icons {
        let textures = Icon::ALL
            .iter()
            .filter_map(|(icon, bytes)| {
                let picture = eframe::icon_data::from_png_bytes(bytes).ok()?;
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [picture.width as usize, picture.height as usize],
                    &picture.rgba,
                );
                let name = format!("simq-icon-{icon:?}");
                Some((
                    *icon,
                    ctx.load_texture(name, image, egui::TextureOptions::LINEAR),
                ))
            })
            .collect();
        Icons { textures }
    }

    fn texture(&self, icon: Icon) -> Option<&egui::TextureHandle> {
        self.textures
            .iter()
            .find(|(i, _)| *i == icon)
            .map(|(_, texture)| texture)
    }

    /// The icon as an image in the given colour.
    pub fn image(&self, icon: Icon, size: f32, color: Color32) -> Option<egui::Image<'static>> {
        let texture = self.texture(icon)?;
        Some(
            egui::Image::from_texture(egui::load::SizedTexture::new(
                texture.id(),
                egui::vec2(size, size),
            ))
            .tint(color),
        )
    }

    /// A button with an icon and, if `text` is not empty, a label. Without
    /// the icon (it could not be loaded) the button shows `fallback` instead.
    pub fn button(
        &self,
        ui: &egui::Ui,
        icon: Icon,
        text: &str,
        fallback: &str,
    ) -> egui::Button<'static> {
        let color = ui.visuals().widgets.inactive.fg_stroke.color;
        match self.image(icon, 14.0, color) {
            Some(image) if text.is_empty() => egui::Button::image(image),
            Some(image) => egui::Button::image_and_text(image, text.to_owned()),
            None if text.is_empty() => egui::Button::new(fallback.to_owned()),
            None => egui::Button::new(text.to_owned()),
        }
    }

    /// Shows the icon as a small picture in the weak text colour.
    pub fn show(&self, ui: &mut egui::Ui, icon: Icon, size: f32) {
        if let Some(image) = self.image(icon, size, ui.visuals().weak_text_color()) {
            ui.add(image);
        }
    }
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
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

pub fn status_pill(ui: &mut egui::Ui, status: Status) -> Response {
    let color = status_color(status, ui.visuals().dark_mode);
    pill(ui, status.label(), color)
}

/// The main action in a view: a slanted racing-red button.
struct Primary<'a> {
    text: &'a str,
    icon: Option<egui::Image<'static>>,
}

impl egui::Widget for Primary<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        /// How far the short edges lean.
        const SLANT: f32 = 7.0;
        const ICON: f32 = 13.0;
        let enabled = ui.is_enabled();
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_owned(), semibold_font(14.0), Color32::WHITE);
        let icon_width = if self.icon.is_some() { ICON + 6.0 } else { 0.0 };
        let size = egui::vec2(
            galley.size().x + icon_width + 30.0 + SLANT,
            galley.size().y.max(18.0) + 8.0,
        );
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let text = self.text.to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text.as_str())
        });
        if ui.is_rect_visible(rect) {
            let fill = if !enabled {
                ui.visuals().widgets.inactive.bg_fill
            } else if response.is_pointer_button_down_on() {
                ACCENT_PRESSED
            } else if response.hovered() {
                ACCENT_HOVER
            } else {
                ACCENT
            };
            let ink = if enabled {
                Color32::WHITE
            } else {
                ui.visuals().weak_text_color()
            };
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(rect.left() + SLANT, rect.top()),
                    egui::pos2(rect.right(), rect.top()),
                    egui::pos2(rect.right() - SLANT, rect.bottom()),
                    egui::pos2(rect.left(), rect.bottom()),
                ],
                fill,
                Stroke::NONE,
            ));
            let mut x = rect.center().x - (galley.size().x + icon_width) / 2.0;
            if let Some(icon) = self.icon {
                let at = egui::Rect::from_min_size(
                    egui::pos2(x, rect.center().y - ICON / 2.0),
                    egui::vec2(ICON, ICON),
                );
                icon.tint(ink).paint_at(ui, at);
                x += icon_width;
            }
            ui.painter().galley_with_override_text_color(
                egui::pos2(x, rect.center().y - galley.size().y / 2.0),
                galley,
                ink,
            );
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

/// Adds a primary button.
pub fn add_primary(ui: &mut egui::Ui, enabled: bool, text: &str) -> Response {
    ui.add_enabled(enabled, Primary { text, icon: None })
}

/// Adds a primary button with an icon before the text.
pub fn add_primary_icon(
    ui: &mut egui::Ui,
    enabled: bool,
    icons: &Icons,
    icon: Icon,
    text: &str,
) -> Response {
    let icon = icons.image(icon, 13.0, Color32::WHITE);
    ui.add_enabled(enabled, Primary { text, icon })
}

/// One of the main views in the top bar: upper-case, with a red line under
/// the one that is open.
pub fn nav_tab(ui: &mut egui::Ui, selected: bool, text: &str) -> Response {
    let color = if selected {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_uppercase(), semibold_font(13.5), color);
    let size = galley.size() + egui::vec2(20.0, 14.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let label = text.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    if ui.is_rect_visible(rect) {
        let ink = if response.hovered() && !selected {
            ui.visuals().text_color()
        } else {
            color
        };
        ui.painter().galley_with_override_text_color(
            rect.center() - galley.size() / 2.0,
            galley,
            ink,
        );
        if selected {
            let line = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 8.0, rect.bottom() - 3.0),
                egui::pos2(rect.right() - 8.0, rect.bottom()),
            );
            ui.painter().rect_filled(line, CornerRadius::ZERO, ACCENT);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The finish line of a panel: a red line along its bottom edge with a
/// chequered flag pattern fading in at the right end.
pub fn finish_line(ui: &egui::Ui) {
    let rect = ui.clip_rect();
    let painter = ui.painter();
    const CELL: f32 = 4.0;
    let ink = ui.visuals().text_color();
    for column in 0..40 {
        for row in 0..2 {
            if (column + row) % 2 != 0 {
                continue;
            }
            let alpha = (150.0 * (1.0 - column as f32 / 40.0)) as u8;
            let right = rect.right() - column as f32 * CELL;
            let bottom = rect.bottom() - 2.0 - row as f32 * CELL;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(right - CELL, bottom - CELL),
                    egui::pos2(right, bottom),
                ),
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), alpha / 3),
            );
        }
    }
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            rect.right_bottom(),
        ),
        CornerRadius::ZERO,
        ACCENT,
    );
}

/// A small filled circle, used as a status light.
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A small upper-case heading above a group of fields.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // A short red bar in front, like a timing-screen marker.
        let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, 11.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius::ZERO, ACCENT);
        ui.label(
            RichText::new(title.to_uppercase())
                .font(semibold_font(11.5))
                .color(ui.visuals().weak_text_color()),
        );
    });
    ui.add_space(1.0);
}

/// A subtle card around a group of content.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(6))
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
