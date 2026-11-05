use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2, Rect, Response, Sense,
    Stroke, TextStyle, Theme, Ui, Vec2, epaint::Shadow,
};

pub struct Palette {
    pub sidebar: Color32,
    pub content: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub faint: Color32,
    pub hairline: Color32,
    pub hover: Color32,
    pub accent: Color32,
}

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// The palette for the current macOS appearance; `sync` updates it once per frame.
pub fn p() -> &'static Palette {
    if DARK_MODE.load(Relaxed) { &DARK } else { &LIGHT }
}

pub fn sync(ctx: &egui::Context) {
    DARK_MODE.store(ctx.style().visuals.dark_mode, Relaxed);
}

const DARK: Palette = Palette {
    sidebar: Color32::from_rgb(22, 22, 24),
    content: Color32::from_rgb(14, 14, 15),
    text: Color32::from_rgb(236, 236, 236),
    secondary: Color32::from_rgb(142, 142, 147),
    faint: Color32::from_rgb(92, 92, 97),
    hairline: Color32::from_rgb(38, 38, 40),
    hover: Color32::from_rgb(32, 32, 35),
    accent: Color32::from_rgb(244, 114, 182),
};

const LIGHT: Palette = Palette {
    sidebar: Color32::from_rgb(246, 246, 244),
    content: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(17, 17, 17),
    secondary: Color32::from_rgb(110, 110, 115),
    faint: Color32::from_rgb(168, 168, 170),
    hairline: Color32::from_rgb(230, 230, 227),
    hover: Color32::from_rgb(236, 236, 233),
    accent: Color32::from_rgb(236, 72, 153),
};

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, path, family) in [
        ("sf", "/System/Library/Fonts/SFNS.ttf", FontFamily::Proportional),
        ("sfmono", "/System/Library/Fonts/SFNSMono.ttf", FontFamily::Monospace),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), FontData::from_owned(bytes).into());
            fonts.families.entry(family).or_default().insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
    for (theme, p) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        ctx.style_mut_of(theme, |s| {
            s.text_styles = [
                (TextStyle::Small, FontId::proportional(11.0)),
                (TextStyle::Body, FontId::proportional(13.5)),
                (TextStyle::Button, FontId::proportional(13.5)),
                (TextStyle::Heading, FontId::proportional(26.0)),
                (TextStyle::Monospace, FontId::monospace(12.0)),
            ]
            .into();
            s.spacing.item_spacing = Vec2::new(8.0, 6.0);
            s.spacing.button_padding = Vec2::new(10.0, 5.0);
            s.spacing.scroll.floating = true;
            s.spacing.scroll.bar_width = 6.0;

            let v = &mut s.visuals;
            v.dark_mode = theme == Theme::Dark;
            v.override_text_color = Some(p.text);
            v.panel_fill = p.content;
            v.window_fill = p.content;
            v.extreme_bg_color = p.content;
            v.faint_bg_color = p.sidebar;
            v.window_stroke = Stroke::new(1.0f32, p.hairline);
            v.window_corner_radius = CornerRadius::same(14);
            v.window_shadow = Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(40) };
            v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(24) };
            v.selection.bg_fill = p.accent.gamma_multiply(0.22);
            v.selection.stroke = Stroke::new(1.0f32, p.accent);
            v.text_cursor.stroke = Stroke::new(1.5f32, p.text);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = CornerRadius::same(7);
                w.bg_stroke = Stroke::NONE;
                w.fg_stroke = Stroke::new(1.0f32, p.text);
                w.expansion = 0.0;
            }
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0f32, p.hairline);
            v.widgets.inactive.bg_fill = p.hover;
            v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            v.widgets.hovered.weak_bg_fill = p.hover;
            v.widgets.hovered.bg_fill = p.secondary;
            v.widgets.active.weak_bg_fill = p.hover;
            v.widgets.active.bg_fill = p.text;
        });
    }
}

/// A frameless text field with a single hairline underneath.
pub fn field(ui: &mut Ui, text: &mut String, hint: &str, password: bool) -> Response {
    let p = p();
    let resp = ui.add(
        egui::TextEdit::singleline(text)
            .frame(false)
            .password(password)
            .hint_text(egui::RichText::new(hint).color(p.faint))
            .font(FontId::proportional(13.5))
            .margin(Vec2::new(0.0, 4.0))
            .desired_width(f32::INFINITY),
    );
    let color = if resp.has_focus() { p.text } else { p.hairline };
    ui.painter().hline(resp.rect.x_range(), resp.rect.bottom(), Stroke::new(1.0f32, color));
    resp
}

pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(spaced(text.to_uppercase(), 10.5, p().faint, 0.8));
}

pub fn spaced(text: String, size: f32, color: Color32, spacing: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text,
        0.0,
        egui::TextFormat {
            font_id: FontId::proportional(size),
            color,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    job
}

pub fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let p = p();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 40.0),
        if enabled { Sense::click() } else { Sense::hover() },
    );
    let fill = if !enabled {
        p.faint
    } else if resp.hovered() {
        p.accent.gamma_multiply(0.85)
    } else {
        p.accent
    };
    ui.painter().rect_filled(rect, CornerRadius::same(20), fill);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(14.0), Color32::WHITE);
    resp.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}

pub fn shortcut_button(ui: &mut Ui, label: &str, recording: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    let painter = ui.painter();
    if recording {
        painter.rect_filled(rect, CornerRadius::same(9), p().content);
        painter.rect_stroke(rect, CornerRadius::same(9), Stroke::new(1.0f32, p().accent), egui::StrokeKind::Inside);
    } else {
        painter.rect_filled(rect, CornerRadius::same(9), if resp.hovered() { p().hairline } else { p().hover });
    }
    let color = if recording { p().accent } else { p().text };
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(14.0), color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = p();
    let (outer, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::hover());
    ui.painter().rect_filled(outer, CornerRadius::same(9), p.hover);
    let w = outer.width() / options.len() as f32;
    let mut changed = false;
    for (i, (opt, label)) in options.iter().enumerate() {
        let rect =
            Rect::from_min_size(outer.min + Vec2::new(w * i as f32, 0.0), Vec2::new(w, outer.height())).shrink(3.0);
        let resp = ui.interact(rect, ui.id().with(label), Sense::click());
        let on = *value == *opt;
        if on {
            ui.painter().rect_filled(rect, CornerRadius::same(7), p.content);
        }
        let color = if on { p.text } else { p.secondary };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, *label, FontId::proportional(12.5), color);
        if resp.clicked() && !on {
            *value = *opt;
            changed = true;
        }
    }
    changed
}

/// A 28 pt square hit target with a hover wash, drawing `glyph` centered in it.
pub fn icon_button(ui: &mut Ui, tooltip: &str, glyph: impl FnOnce(&Painter, Pos2, Color32)) -> Response {
    let p = p();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(7), p.hover);
    }
    glyph(ui.painter(), rect.center(), if resp.hovered() { p.text } else { p.secondary });
    resp.on_hover_text(tooltip)
}

pub fn mic(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4f32, color);
    painter.rect_stroke(
        Rect::from_center_size(c - Vec2::new(0.0, 2.5), Vec2::new(5.0, 9.0)),
        CornerRadius::same(3),
        s,
        egui::StrokeKind::Middle,
    );
    let arc: Vec<Pos2> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / 12.0;
            c + Vec2::new(-4.5 * a.cos(), 4.5 * a.sin() - 1.0)
        })
        .collect();
    painter.add(egui::Shape::line(arc, s));
    painter.line_segment([c + Vec2::new(0.0, 3.5), c + Vec2::new(0.0, 6.0)], s);
}

pub fn speaker(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4f32, color);
    let body = vec![
        c + Vec2::new(-6.0, -2.0),
        c + Vec2::new(-3.5, -2.0),
        c + Vec2::new(0.0, -5.0),
        c + Vec2::new(0.0, 5.0),
        c + Vec2::new(-3.5, 2.0),
        c + Vec2::new(-6.0, 2.0),
    ];
    painter.add(egui::Shape::closed_line(body, s));
    let arc: Vec<Pos2> = (0..=10)
        .map(|i| {
            let a = -0.9 + 1.8 * i as f32 / 10.0;
            c + Vec2::new(1.5 + 4.0 * a.cos(), 4.0 * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arc, s));
}

pub fn sliders(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4f32, color);
    for (dy, knob) in [(-3.5, 2.5), (3.5, -2.5)] {
        painter.line_segment([c + Vec2::new(-6.5, dy), c + Vec2::new(6.5, dy)], s);
        painter.circle_filled(c + Vec2::new(knob, dy), 2.2, color);
    }
}

pub fn leave(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4f32, color);
    let arc: Vec<Pos2> = (0..=16)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + 0.6 + (2.0 * std::f32::consts::PI - 1.2) * i as f32 / 16.0;
            c + Vec2::new(5.5 * a.cos(), 5.5 * a.sin() + 0.5)
        })
        .collect();
    painter.add(egui::Shape::line(arc, s));
    painter.line_segment([c + Vec2::new(0.0, -6.0), c + Vec2::new(0.0, -0.5)], s);
}

pub fn lock(painter: &Painter, c: Pos2, color: Color32) {
    painter.rect_filled(
        Rect::from_center_size(c + Vec2::new(0.0, 1.5), Vec2::new(7.0, 5.0)),
        CornerRadius::same(1),
        color,
    );
    let arc: Vec<Pos2> = (0..=8)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / 8.0;
            c + Vec2::new(2.3 * a.cos(), -1.0 - 2.5 * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(arc, Stroke::new(1.2f32, color)));
}

pub fn slash(painter: &Painter, c: Pos2, color: Color32) {
    painter.line_segment([c + Vec2::new(-6.0, -6.0), c + Vec2::new(6.0, 6.0)], Stroke::new(1.6f32, color));
}
