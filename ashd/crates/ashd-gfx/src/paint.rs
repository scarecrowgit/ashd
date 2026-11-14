//! Drawing helpers in logical ("design") px. Everything is converted to physical px and
//! snapped to whole pixels, so 1 px lines stay 1 crisp device pixel (≥ 1) at any scale.

use vello::{
    Scene,
    kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Stroke},
    peniko::{Fill, Mix},
};

use crate::{color, icon::Icon, text::TextLayout};
use ashd_tokens::Rgba;

/// Wraps a scene with a logical → physical `scale`.
pub struct Painter<'a> {
    pub scene: &'a mut Scene,
    pub scale: f64,
}

impl<'a> Painter<'a> {
    pub fn new(scene: &'a mut Scene, scale: f64) -> Self {
        Self { scene, scale }
    }

    /// Logical → physical, snapped.
    pub fn px(&self, v: f64) -> f64 {
        (v * self.scale).round()
    }

    /// Line width in physical px: at least 1.
    pub fn line_px(&self, w: f64) -> f64 {
        (w * self.scale).round().max(1.0)
    }

    fn snap_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Rect {
        let x0 = self.px(x);
        let y0 = self.px(y);
        let x1 = self.px(x + w).max(x0 + if w > 0.0 { 1.0 } else { 0.0 });
        let y1 = self.px(y + h).max(y0 + if h > 0.0 { 1.0 } else { 0.0 });
        Rect::new(x0, y0, x1, y1)
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: Rgba) {
        let r = self.snap_rect(x, y, w, h);
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color(c), None, &r);
    }

    pub fn fill_round_rect(&mut self, x: f64, y: f64, w: f64, h: f64, radius: f64, c: Rgba) {
        if radius <= 0.0 {
            return self.fill_rect(x, y, w, h, c);
        }
        let r = RoundedRect::from_rect(self.snap_rect(x, y, w, h), radius * self.scale);
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color(c), None, &r);
    }

    /// CSS border inside the box `x, y, w, h` (border-box sizing).
    pub fn border(&mut self, x: f64, y: f64, w: f64, h: f64, width: f64, radius: f64, c: Rgba) {
        let outer = self.snap_rect(x, y, w, h);
        let lw = self.line_px(width);
        if radius <= 0.0 {
            let brush = color(c);
            let sides = [
                Rect::new(outer.x0, outer.y0, outer.x1, outer.y0 + lw),
                Rect::new(outer.x0, outer.y1 - lw, outer.x1, outer.y1),
                Rect::new(outer.x0, outer.y0 + lw, outer.x0 + lw, outer.y1 - lw),
                Rect::new(outer.x1 - lw, outer.y0 + lw, outer.x1, outer.y1 - lw),
            ];
            for s in sides {
                self.scene.fill(Fill::NonZero, Affine::IDENTITY, brush, None, &s);
            }
        } else {
            let inset = outer.inset(-lw / 2.0);
            let rr = RoundedRect::from_rect(inset, (radius * self.scale - lw / 2.0).max(0.0));
            self.scene.stroke(&Stroke::new(lw), Affine::IDENTITY, color(c), None, &rr);
        }
    }

    /// Horizontal line of `width` px whose top edge is at `y`.
    pub fn hline(&mut self, x: f64, y: f64, len: f64, width: f64, c: Rgba) {
        let x0 = self.px(x);
        let x1 = self.px(x + len);
        let y0 = self.px(y);
        let r = Rect::new(x0, y0, x1, y0 + self.line_px(width));
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color(c), None, &r);
    }

    /// Vertical line of `width` px whose left edge is at `x`.
    pub fn vline(&mut self, x: f64, y: f64, len: f64, width: f64, c: Rgba) {
        let y0 = self.px(y);
        let y1 = self.px(y + len);
        let x0 = self.px(x);
        let r = Rect::new(x0, y0, x0 + self.line_px(width), y1);
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color(c), None, &r);
    }

    /// Filled circle (not snapped: dots stay round).
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64, c: Rgba) {
        let circle = Circle::new((cx * self.scale, cy * self.scale), r * self.scale);
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color(c), None, &circle);
    }

    /// Stroke an arbitrary logical-px path.
    pub fn stroke_path(&mut self, path: &BezPath, width: f64, c: Rgba) {
        let stroke = Stroke::new(width).with_caps(Cap::Round).with_join(Join::Round);
        self.scene.stroke(&stroke, Affine::scale(self.scale), color(c), None, path);
    }

    /// Text with its line box's top-left at (`x`, `y`). The layout must have been made at
    /// this painter's scale (`Text::layout(.., painter.scale)`).
    pub fn text(&mut self, layout: &TextLayout, x: f64, y: f64) {
        let origin = Affine::translate((self.px(x), self.px(y)));
        for line in layout.layout.lines() {
            for item in line.items() {
                let parley::PositionedLayoutItem::GlyphRun(run) = item else { continue };
                let style = run.style();
                let r = run.run();
                let font = r.font();
                let coords: Vec<i16> = r.normalized_coords().iter().map(|c| c.to_bits()).collect();
                self.scene
                    .draw_glyphs(font)
                    .transform(origin)
                    .font_size(r.font_size())
                    .normalized_coords(&coords)
                    .brush(color(style.brush))
                    .hint(false)
                    .draw(
                        Fill::NonZero,
                        run.positioned_glyphs().map(|g| vello::Glyph { id: g.id, x: g.x, y: g.y }),
                    );
            }
        }
    }

    /// Icon of `size` logical px with its top-left at (`x`, `y`), in colour `c`.
    pub fn icon(&mut self, icon: &Icon, x: f64, y: f64, size: f64, c: Rgba) {
        let k = size * self.scale / icon.view_box;
        let t = Affine::translate((self.px(x), self.px(y))) * Affine::scale(k);
        let brush = color(c);
        for part in &icon.parts {
            if part.fill {
                self.scene.fill(Fill::NonZero, t, brush, None, &part.path);
            }
            if let Some(w) = part.stroke {
                let stroke = Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round);
                self.scene.stroke(&stroke, t, brush, None, &part.path);
            }
        }
    }

    /// Draw `f` into a group with opacity `alpha`, clipped to the whole area `w × h`.
    pub fn with_alpha(&mut self, alpha: f32, w: f64, h: f64, f: impl FnOnce(&mut Painter<'_>)) {
        if alpha >= 0.999 {
            return f(self);
        }
        let clip = Rect::new(0.0, 0.0, w * self.scale, h * self.scale);
        #[allow(deprecated)]
        self.scene.push_layer(Fill::NonZero, Mix::Normal, alpha.max(0.0), Affine::IDENTITY, &clip);
        f(self);
        self.scene.pop_layer();
    }

    /// Clip `f` to a logical rect.
    pub fn clipped(&mut self, x: f64, y: f64, w: f64, h: f64, f: impl FnOnce(&mut Painter<'_>)) {
        let clip = self.snap_rect(x, y, w, h);
        self.scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
        f(self);
        self.scene.pop_layer();
    }

    /// Physical point for a logical one (no snapping).
    pub fn point(&self, x: f64, y: f64) -> Point {
        Point::new(x * self.scale, y * self.scale)
    }
}
