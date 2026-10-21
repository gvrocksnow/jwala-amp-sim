//! Immediate-mode 2D drawing API in logical pixels. Records instanced primitives and soft-shadow
//! jobs; `gpu::Renderer` turns them into draw calls.

use super::text::{self, Atlas, Font, ATLAS};
use bytemuck::{Pod, Zeroable};

pub type Color = [f32; 4];

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}
pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    rgba(r, g, b, 255)
}
/// `c` with alpha `a` (0..255), like GDI+ `Color.FromArgb(a, c)`.
pub fn with_alpha(c: Color, a: u8) -> Color {
    [c[0], c[1], c[2], a as f32 / 255.0]
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub fn inflate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x - dx, self.y - dy, self.w + 2.0 * dx, self.h + 2.0 * dy)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn centered(cx: f32, cy: f32, w: f32, h: f32) -> Rect {
        Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
    }
}

pub const K_RRECT: f32 = 0.0;
pub const K_IMAGE: f32 = 1.0;
pub const K_GLYPH: f32 = 2.0;
pub const K_ELLIPSE: f32 = 3.0;
pub const K_GLOW: f32 = 4.0;
pub const K_GRAD: f32 = 5.0;
pub const K_LINE: f32 = 6.0;
pub const K_TILE: f32 = 7.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Inst {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub color2: [f32; 4],
    pub p: [f32; 4],
    pub q: [f32; 4],
    pub clip: [f32; 4],
}

/// Index into the renderer's image list; `TexId::WHITE` for untextured primitives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TexId(pub usize);
impl TexId {
    pub const WHITE: TexId = TexId(0);
}

pub enum Cmd {
    /// Instances `start..end`, all of one primitive kind and one texture.
    Quads { tex: TexId, kind: u8, start: u32, end: u32 },
    Shadow(usize),
}

/// One light term of a soft shadow: occluder offset, blur sigma (logical px) and strength.
#[derive(Clone, Copy, PartialEq)]
pub struct ShadowTerm {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub strength: f32,
}

/// Instances recorded for an off-screen mask.
#[derive(Default)]
pub struct Layer {
    pub inst: Vec<Inst>,
    pub cmds: Vec<Cmd>,
}

/// A shadow whose masks must be (re)rendered this frame.
pub struct ShadowJob {
    pub slot: usize,
    pub occluders: Layer,
    pub receiver: Option<Layer>,
    pub terms: [ShadowTerm; 3],
}

pub struct Painter<'a> {
    pub layer: Layer,
    pub shadow_jobs: Vec<ShadowJob>,
    pub scale: f32,
    pub size: (f32, f32),
    pub atlas: &'a mut Atlas,
    /// Image sizes, indexed by `TexId`, for tiling.
    pub tex_sizes: &'a [(u32, u32)],
    /// Which shadow slots are up to date for which layout key.
    pub shadow_keys: &'a mut Vec<u64>,
    clip: Vec<[f32; 4]>,
}

impl<'a> Painter<'a> {
    pub fn new(size: (f32, f32), scale: f32, atlas: &'a mut Atlas, tex_sizes: &'a [(u32, u32)], shadow_keys: &'a mut Vec<u64>) -> Self {
        Painter { layer: Layer::default(), shadow_jobs: Vec::new(), scale, size, atlas, tex_sizes, shadow_keys, clip: vec![[-1e9, -1e9, 1e9, 1e9]] }
    }

    fn clip_rect(&self) -> [f32; 4] {
        *self.clip.last().unwrap()
    }

    pub fn push_clip(&mut self, r: Rect) {
        let c = self.clip_rect();
        self.clip.push([c[0].max(r.x), c[1].max(r.y), c[2].min(r.right()), c[3].min(r.bottom())]);
    }
    pub fn pop_clip(&mut self) {
        if self.clip.len() > 1 {
            self.clip.pop();
        }
    }

    fn push(&mut self, tex: TexId, i: Inst) {
        let idx = self.layer.inst.len() as u32;
        let kind = i.p[0] as u8;
        self.layer.inst.push(i);
        if let Some(Cmd::Quads { tex: t, kind: k, end, .. }) = self.layer.cmds.last_mut() {
            if *t == tex && *k == kind && *end == idx {
                *end += 1;
                return;
            }
        }
        self.layer.cmds.push(Cmd::Quads { tex, kind, start: idx, end: idx + 1 });
    }

    fn inst(&self, r: Rect, kind: f32, color: Color) -> Inst {
        Inst { rect: [r.x, r.y, r.w, r.h], uv: [0.0, 0.0, 1.0, 1.0], color, color2: [0.0; 4], p: [kind, 0.0, 0.0, 0.0], q: [0.0; 4], clip: self.clip_rect() }
    }

    // ------------------------------------------------------------------ shapes

    pub fn rrect(&mut self, r: Rect, radius: f32, color: Color) {
        let mut i = self.inst(r, K_RRECT, color);
        i.p[1] = radius;
        self.push(TexId::WHITE, i);
    }

    pub fn fill(&mut self, r: Rect, color: Color) {
        self.rrect(r, 0.0, color);
    }

    pub fn rrect_stroke(&mut self, r: Rect, radius: f32, width: f32, color: Color) {
        let mut i = self.inst(r, K_RRECT, color);
        i.p[1] = radius;
        i.p[2] = width;
        self.push(TexId::WHITE, i);
    }

    /// Vertical gradient from `top` to `bottom` across `r` (GDI+ LinearGradientBrush at 90°).
    pub fn grad(&mut self, r: Rect, radius: f32, top: Color, bottom: Color) {
        let mut i = self.inst(r, K_GRAD, top);
        i.color2 = bottom;
        i.p[1] = radius;
        i.q = [r.y, r.bottom(), 0.0, 0.0];
        self.push(TexId::WHITE, i);
    }

    pub fn ellipse(&mut self, r: Rect, color: Color) {
        let i = self.inst(r, K_ELLIPSE, color);
        self.push(TexId::WHITE, i);
    }

    pub fn ellipse_stroke(&mut self, r: Rect, width: f32, color: Color) {
        let mut i = self.inst(r, K_ELLIPSE, color);
        i.p[2] = width;
        self.push(TexId::WHITE, i);
    }

    /// Radial glow: `center` fading to `edge` at the ellipse boundary; `focus` keeps a plateau.
    pub fn glow(&mut self, r: Rect, center: Color, edge: Color, focus: f32) {
        let mut i = self.inst(r, K_GLOW, center);
        i.color2 = edge;
        i.p[3] = focus;
        self.push(TexId::WHITE, i);
    }

    /// A line with round caps.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
        let pad = width / 2.0 + 1.0;
        let r = Rect::new(x0.min(x1) - pad, y0.min(y1) - pad, (x1 - x0).abs() + 2.0 * pad, (y1 - y0).abs() + 2.0 * pad);
        let mut i = self.inst(r, K_LINE, color);
        i.p[2] = width;
        i.q = [x0, y0, x1, y1];
        self.push(TexId::WHITE, i);
    }

    /// An arc as a chain of short round-capped segments (angles in degrees, 0 = right, clockwise).
    pub fn arc(&mut self, cx: f32, cy: f32, radius: f32, start: f32, sweep: f32, width: f32, color: Color) {
        let n = ((sweep.abs() / 8.0).ceil() as usize).max(2);
        let pt = |k: usize| {
            let a = (start + sweep * k as f32 / n as f32).to_radians();
            (cx + radius * a.cos(), cy + radius * a.sin())
        };
        for k in 0..n {
            let (a, b) = (pt(k), pt(k + 1));
            self.line(a.0, a.1, b.0, b.1, width, color);
        }
    }

    // ------------------------------------------------------------------ images

    pub fn image(&mut self, tex: TexId, r: Rect) {
        self.image_tinted(tex, r, [1.0; 4]);
    }

    /// Image multiplied by `tint` (premultiplied: use [a, a, a, a] to fade).
    pub fn image_tinted(&mut self, tex: TexId, r: Rect, tint: Color) {
        let i = self.inst(r, K_IMAGE, tint);
        self.push(tex, i);
    }

    /// Image clipped to a rounded rect.
    pub fn image_rounded(&mut self, tex: TexId, r: Rect, uv: [f32; 4], radius: f32) {
        let mut i = self.inst(r, K_IMAGE, [1.0; 4]);
        i.uv = uv;
        i.p[1] = radius;
        self.push(tex, i);
    }

    /// Repeats `tex` at `scale` over `r`, clipped to a rounded rect.
    pub fn tile(&mut self, tex: TexId, r: Rect, scale: f32, radius: f32) {
        let (tw, th) = self.tex_sizes.get(tex.0).copied().unwrap_or((1, 1));
        let mut i = self.inst(r, K_TILE, [1.0; 4]);
        i.uv = [r.x / (tw as f32 * scale), r.y / (th as f32 * scale), r.right() / (tw as f32 * scale), r.bottom() / (th as f32 * scale)];
        i.p[1] = radius;
        self.push(tex, i);
    }

    /// Draws `tex` scaled to cover `r` (CSS background-size: cover).
    pub fn cover(&mut self, tex: TexId, r: Rect) {
        let (tw, th) = self.tex_sizes.get(tex.0).copied().unwrap_or((1, 1));
        let s = (r.w / tw as f32).max(r.h / th as f32);
        let (w, h) = (tw as f32 * s, th as f32 * s);
        self.push_clip(r);
        self.image(tex, Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h));
        self.pop_clip();
    }

    // ------------------------------------------------------------------ text

    /// Draws `s` with its left end at `x` and baseline at `baseline`. `grad` = (top, bottom colour, y0, y1).
    #[allow(clippy::too_many_arguments)]
    fn glyphs(&mut self, font: Font, pt: f32, s: &str, x: f32, baseline: f32, color: Color, grad: Option<(Color, f32, f32)>, skew: f32, spacing: f32) {
        let phys = text::pt_to_px(pt) * self.scale;
        let adv = text::advances(font, pt, s);
        let mut pen = x;
        for (c, a) in s.chars().zip(adv) {
            let g = self.atlas.glyph(font, c, phys);
            if g.rect[2] > 0 {
                let r = Rect::new(
                    (pen * self.scale + g.off_x).round() / self.scale,
                    (baseline * self.scale + g.off_y).round() / self.scale,
                    g.rect[2] as f32 / self.scale,
                    g.rect[3] as f32 / self.scale,
                );
                let at = ATLAS as f32;
                let mut i = self.inst(r, K_GLYPH, color);
                i.uv = [g.rect[0] as f32 / at, g.rect[1] as f32 / at, (g.rect[0] + g.rect[2]) as f32 / at, (g.rect[1] + g.rect[3]) as f32 / at];
                i.p[3] = skew;
                if let Some((c2, y0, y1)) = grad {
                    i.color2 = c2;
                    i.q = [y0, y1, 0.0, 0.0];
                }
                self.push(TexId::WHITE, i);
            }
            pen += a + spacing;
        }
    }

    /// Text with its top-left at (x, y) like GDI+ `DrawString(s, font, brush, x, y)`.
    pub fn text(&mut self, font: Font, pt: f32, s: &str, x: f32, y: f32, color: Color) {
        let m = text::line_metrics(font, pt);
        self.glyphs(font, pt, s, x, y + m.ascent, color, None, 0.0, 0.0);
    }

    /// Text aligned in a rect: `h`/`v` are 0 = near, 0.5 = centre, 1 = far. No wrapping.
    #[allow(clippy::too_many_arguments)]
    pub fn text_in(&mut self, font: Font, pt: f32, s: &str, r: Rect, h: f32, v: f32, color: Color) {
        let m = text::line_metrics(font, pt);
        let w = text::measure(font, pt, s);
        let x = r.x + (r.w - w) * h;
        let base = r.y + (r.h - m.height()) * v + m.ascent;
        self.glyphs(font, pt, s, x, base, color, None, 0.0, 0.0);
    }

    /// Centred text, trimmed with an ellipsis to fit the rect's width.
    pub fn text_fit(&mut self, font: Font, pt: f32, s: &str, r: Rect, h: f32, color: Color) {
        let mut t = s.to_string();
        if text::measure(font, pt, &t) > r.w {
            while !t.is_empty() && text::measure(font, pt, &format!("{t}…")) > r.w {
                t.pop();
            }
            t.push('…');
        }
        self.text_in(font, pt, &t, r, h, 0.5, color);
    }

    /// Text spread across exactly `width` by adding even letter spacing.
    pub fn text_spaced(&mut self, font: Font, pt: f32, s: &str, x: f32, y: f32, width: f32, color: Color) {
        let m = text::line_metrics(font, pt);
        let n = s.chars().count();
        let natural = text::measure(font, pt, s);
        let gap = if n > 1 { (width - natural) / (n - 1) as f32 } else { 0.0 };
        let x = if n > 1 { x } else { x + (width - natural) / 2.0 };
        self.glyphs(font, pt, s, x, y + m.ascent, color, None, 0.0, gap);
    }

    /// The "Jwala" script logo with drop shadow and gold gradient (Gfx.Logo in the C# app).
    pub fn logo(&mut self, r: Rect, pt: f32, h_align: f32) {
        let s = "Jwala";
        let m = text::line_metrics(Font::Script, pt);
        let w = text::measure(Font::Script, pt, s);
        let x = r.x + (r.w - w) * h_align + 4.0;
        let base = r.y + (r.h - m.height()) / 2.0 + m.ascent;
        let skew = 0.18; // GDI+ synthesises italic for this face
        self.glyphs(Font::Script, pt, s, x + pt / 14.0, base + pt / 11.0, rgba(0, 0, 0, 170), None, skew, 0.0);
        let (top, bottom) = (rgb(255, 240, 200), rgb(176, 128, 52));
        self.glyphs(Font::Script, pt, s, x, base, top, Some((bottom, r.y, r.bottom())), skew, 0.0);
    }

    // ------------------------------------------------------------------ shadows

    /// Soft shadow cast by whatever `occluders` draws, optionally only onto `receiver`'s pixels.
    /// The masks are rendered and blurred on the GPU only when `key` changes (layout), and the
    /// cached result is composited every frame at this point in the draw order.
    pub fn shadow(&mut self, slot: usize, key: u64, terms: [ShadowTerm; 3], occluders: impl FnOnce(&mut Painter), receiver: Option<&dyn Fn(&mut Painter)>) {
        if self.shadow_keys.len() <= slot {
            self.shadow_keys.resize(slot + 1, u64::MAX);
        }
        if self.shadow_keys[slot] != key {
            self.shadow_keys[slot] = key;
            let saved = std::mem::take(&mut self.layer);
            occluders(self);
            let occ = std::mem::replace(&mut self.layer, Layer::default());
            let rec = receiver.map(|f| {
                f(self);
                std::mem::take(&mut self.layer)
            });
            self.layer = saved;
            self.shadow_jobs.push(ShadowJob { slot, occluders: occ, receiver: rec, terms });
        }
        self.layer.cmds.push(Cmd::Shadow(slot));
    }
}
