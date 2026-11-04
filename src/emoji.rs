use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use core_foundation::attributed_string::CFMutableAttributedString;
use core_foundation::base::{CFRange, TCFType};
use core_foundation::string::CFString;
use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_text::line::CTLine;
use core_text::string_attributes::kCTFontAttributeName;
use eframe::egui::{self, Color32, FontId, Galley, Painter, Pos2, Rect, Sense, TextureHandle, Ui, Vec2};
use unicode_segmentation::UnicodeSegmentation;

const RASTER_PT: f64 = 64.0;

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<TextureHandle>>> = RefCell::default();
}

fn is_emoji(grapheme: &str) -> bool {
    grapheme
        .chars()
        .any(|c| matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE0F))
}

fn has_emoji(text: &str) -> bool {
    text.chars().any(|c| c as u32 >= 0x2300) && text.graphemes(true).any(is_emoji)
}

/// Draws the grapheme with Apple Color Emoji through CoreText; egui itself can only render monochrome outlines.
fn rasterize(grapheme: &str) -> Option<egui::ColorImage> {
    let font = core_text::font::new_from_name("Apple Color Emoji", RASTER_PT).ok()?;
    let mut text = CFMutableAttributedString::new();
    text.replace_str(&CFString::new(grapheme), CFRange::init(0, 0));
    let len = text.char_len();
    text.set_attribute(CFRange::init(0, len), unsafe { kCTFontAttributeName }, &font);
    let line = CTLine::new_with_attributed_string(text.as_concrete_TypeRef());
    let b = line.get_typographic_bounds();
    let (w, h) = (b.width.ceil() as usize, (b.ascent + b.descent).ceil() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut ctx = CGContext::create_bitmap_context(
        None,
        w,
        h,
        8,
        w * 4,
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaPremultipliedLast,
    );
    ctx.set_text_position(0.0, b.descent);
    line.draw(&ctx);
    Some(egui::ColorImage::from_rgba_premultiplied([w, h], ctx.data()))
}

fn texture(ctx: &egui::Context, grapheme: &str) -> Option<TextureHandle> {
    CACHE.with_borrow_mut(|cache| {
        cache
            .entry(grapheme.to_string())
            .or_insert_with(|| {
                rasterize(grapheme).map(|img| ctx.load_texture(format!("emoji:{grapheme}"), img, Default::default()))
            })
            .clone()
    })
}

enum Part {
    Text(Arc<Galley>, f32),
    Emoji(TextureHandle, f32, Vec2),
}

/// A single line of text with color emoji laid out inline.
pub struct Line {
    parts: Vec<Part>,
    pub size: Vec2,
}

impl Line {
    /// `layout` shapes plain text runs, so callers keep their own font, color and letter spacing.
    pub fn new(ctx: &egui::Context, text: &str, font_size: f32, layout: impl Fn(String) -> Arc<Galley>) -> Self {
        let mut parts = Vec::new();
        let mut x = 0.0;
        let mut height: f32 = 0.0;
        let mut run = String::new();
        let flush = |run: &mut String, parts: &mut Vec<Part>, x: &mut f32, height: &mut f32| {
            if !run.is_empty() {
                let g = layout(std::mem::take(run));
                *height = height.max(g.size().y);
                parts.push(Part::Text(g.clone(), *x));
                *x += g.size().x;
            }
        };
        if has_emoji(text) {
            for g in text.graphemes(true) {
                match is_emoji(g).then(|| texture(ctx, g)).flatten() {
                    Some(tex) => {
                        flush(&mut run, &mut parts, &mut x, &mut height);
                        let [w, h] = tex.size();
                        let side = font_size * 1.2;
                        let size = Vec2::new(side * w as f32 / h as f32, side);
                        parts.push(Part::Emoji(tex, x, size));
                        x += size.x + 1.0;
                        height = height.max(size.y);
                    }
                    None => run.push_str(g),
                }
            }
        } else {
            run.push_str(text);
        }
        flush(&mut run, &mut parts, &mut x, &mut height);
        Self { parts, size: Vec2::new(x, height) }
    }

    pub fn paint(&self, painter: &Painter, left_center: Pos2, color: Color32) {
        for part in &self.parts {
            match part {
                Part::Text(g, x) => {
                    painter.galley(Pos2::new(left_center.x + x, left_center.y - g.size().y / 2.0), g.clone(), color)
                }
                Part::Emoji(tex, x, size) => {
                    let rect = Rect::from_min_size(Pos2::new(left_center.x + x, left_center.y - size.y / 2.0), *size);
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    painter.image(tex.id(), rect, uv, Color32::WHITE);
                }
            }
        }
    }
}

/// Text runs are laid out with a placeholder color so `paint` decides the color, e.g. on hover.
pub fn line(ui: &Ui, text: &str, font: FontId) -> Line {
    let size = font.size;
    Line::new(ui.ctx(), text, size, |s| ui.painter().layout_no_wrap(s, font.clone(), Color32::PLACEHOLDER))
}

/// Label replacement for single-line user content.
pub fn label(ui: &mut Ui, text: &str, font: FontId, color: Color32) -> egui::Response {
    let line = line(ui, text, font);
    let (rect, resp) = ui.allocate_exact_size(line.size, Sense::hover());
    line.paint(ui.painter(), rect.left_center(), color);
    resp
}

/// Wrapping paragraph with inline emoji, for chat messages.
pub fn paragraph(ui: &mut Ui, text: &str, font: FontId, color: Color32) {
    if !has_emoji(text) {
        ui.label(egui::RichText::new(text).font(font).color(color));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for word in text.split_inclusive(' ') {
            label(ui, word, font.clone(), color);
        }
    });
}
