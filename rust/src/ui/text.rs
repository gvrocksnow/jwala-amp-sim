//! Text: the embedded stand-ins for the Windows fonts the C# UI used (see `crate::fonts`),
//! rasterised with fontdue into a glyph atlas texture.
//! Sizes are in points like GDI+ (1 pt = 96/72 logical px); glyphs are rasterised at physical size.

use crate::fonts::Face;
use fontdue::Font as FdFont;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Font {
    Regular,
    Bold,
    Semibold,
    Light,
    ArialBold,
}

const FACES: [(Font, Face); 5] = [
    (Font::Regular, Face::Regular),
    (Font::Bold, Face::Bold),
    (Font::Semibold, Face::Semibold),
    (Font::Light, Face::Light),
    (Font::ArialBold, Face::ArialBold),
];

pub struct Fonts {
    fonts: HashMap<Font, FdFont>,
    symbol: Option<FdFont>,
}

impl Fonts {
    fn load() -> Fonts {
        let mut fonts = HashMap::new();
        // anything missing falls back to the regular face (or the first face found)
        let mut fallback: Option<FdFont> = None;
        for (id, face) in FACES {
            if let Some(f) = crate::fonts::load(face) {
                if fallback.is_none() {
                    fallback = Some(f.clone());
                }
                fonts.insert(id, f);
            }
        }
        for (id, _) in FACES {
            if !fonts.contains_key(&id) {
                if let Some(f) = fallback.clone() {
                    fonts.insert(id, f);
                }
            }
        }
        Fonts { fonts, symbol: crate::fonts::load(Face::Symbol) }
    }

    pub fn get() -> &'static Fonts {
        static F: OnceLock<Fonts> = OnceLock::new();
        F.get_or_init(Fonts::load)
    }

    /// The face that has `c`, preferring `f`. `None` if no font is available at all.
    fn face(&self, f: Font, c: char) -> Option<(&FdFont, bool)> {
        let primary = self.fonts.get(&f)?;
        if primary.lookup_glyph_index(c) != 0 || c == ' ' {
            return Some((primary, false));
        }
        match &self.symbol {
            Some(s) if s.lookup_glyph_index(c) != 0 => Some((s, true)),
            _ => Some((primary, false)),
        }
    }
}

pub fn pt_to_px(pt: f32) -> f32 {
    pt * 96.0 / 72.0
}

/// Vertical metrics in logical px.
#[derive(Clone, Copy)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

impl LineMetrics {
    pub fn height(&self) -> f32 {
        self.ascent - self.descent
    }
}

pub fn line_metrics(font: Font, pt: f32) -> LineMetrics {
    let px = pt_to_px(pt);
    let face = FACES.iter().find(|(f, _)| *f == font).map(|(_, face)| *face);
    if let Some((ascent, descent)) = face.and_then(crate::fonts::line_metrics_em) {
        return LineMetrics { ascent: ascent * px, descent: descent * px };
    }
    let m = Fonts::get().fonts.get(&font).and_then(|f| f.horizontal_line_metrics(px));
    m.map(|m| LineMetrics { ascent: m.ascent, descent: m.descent }).unwrap_or(LineMetrics { ascent: px * 0.9, descent: -px * 0.25 })
}

/// Advance width of each character (logical px), kerning included.
pub fn advances(font: Font, pt: f32, s: &str) -> Vec<f32> {
    let px = pt_to_px(pt);
    let fonts = Fonts::get();
    let mut out = Vec::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let Some((f, _)) = fonts.face(font, c) else {
            out.push(px * 0.5);
            continue;
        };
        let mut adv = f.metrics(c, px).advance_width;
        if let (Some(p), Some(last)) = (prev, out.last_mut()) {
            if let Some(k) = f.horizontal_kern(p, c, px) {
                *last += k;
            }
        }
        out.push(adv);
        adv = 0.0;
        let _ = adv;
        prev = Some(c);
    }
    out
}

pub fn measure(font: Font, pt: f32, s: &str) -> f32 {
    advances(font, pt, s).iter().sum()
}

/// `s` rasterised `rows` cells tall (cap height) and thresholded, for LED dot-matrix readouts.
pub struct DotMask {
    pub w: usize,
    pub h: usize,
    /// Row of the baseline within the cropped mask (may lie below `h` for glyphs without descenders).
    pub baseline: i32,
    pub cells: Vec<bool>,
}

impl DotMask {
    pub fn on(&self, x: usize, y: usize) -> bool {
        self.cells[y * self.w + x]
    }
}

pub fn dot_mask(font: Font, s: &str, rows: u32) -> DotMask {
    let px = rows as f32 * 1.32;
    let fonts = Fonts::get();
    let m = line_metrics(font, px * 72.0 / 96.0);
    let base = m.ascent.ceil() as i32 + 2;
    let ch = (m.ascent - m.descent).ceil() as i32 + 4;
    let adv = advances(font, px * 72.0 / 96.0, s);
    let cw = adv.iter().sum::<f32>().ceil() as i32 + 8;
    let mut canvas = vec![0u8; (cw * ch).max(1) as usize];
    let mut pen = 2.0f32;
    for (c, a) in s.chars().zip(adv) {
        if let Some((f, _)) = fonts.face(font, c) {
            let (gm, bmp) = f.rasterize(c, px);
            let (ox, oy) = (pen.round() as i32 + gm.xmin, base - gm.ymin - gm.height as i32);
            for y in 0..gm.height as i32 {
                for x in 0..gm.width as i32 {
                    let (tx, ty) = (ox + x, oy + y);
                    if tx >= 0 && ty >= 0 && tx < cw && ty < ch {
                        let d = &mut canvas[(ty * cw + tx) as usize];
                        *d = (*d).max(bmp[(y * gm.width as i32 + x) as usize]);
                    }
                }
            }
        }
        pen += a;
    }
    let lit = |x: i32, y: i32| canvas[(y * cw + x) as usize] > 110;
    let (mut x0, mut y0, mut x1, mut y1) = (cw, ch, -1, -1);
    for y in 0..ch {
        for x in 0..cw {
            if lit(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < 0 {
        return DotMask { w: 0, h: 0, baseline: 0, cells: Vec::new() };
    }
    let (w, h) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let mut cells = Vec::with_capacity(w * h);
    for y in y0..=y1 {
        for x in x0..=x1 {
            cells.push(lit(x, y));
        }
    }
    DotMask { w, h, baseline: base - y0, cells }
}

// ------------------------------------------------------------------ atlas

pub const ATLAS: u32 = 2048;

#[derive(Clone, Copy)]
pub struct Glyph {
    /// Atlas rect in pixels (x, y, w, h); w == 0 for blank glyphs.
    pub rect: [u32; 4],
    /// Offset of the bitmap's top-left from the pen position on the baseline, physical px.
    pub off_x: f32,
    pub off_y: f32,
}

pub struct Atlas {
    pub pixels: Vec<u8>,
    pub dirty: bool,
    glyphs: HashMap<(Font, char, u32), Glyph>,
    x: u32,
    y: u32,
    row_h: u32,
}

impl Default for Atlas {
    fn default() -> Self {
        Atlas { pixels: vec![0; (ATLAS * ATLAS) as usize], dirty: true, glyphs: HashMap::new(), x: 1, y: 1, row_h: 0 }
    }
}

impl Atlas {
    /// Starts over when the atlas is nearly full (call between frames).
    pub fn maintain(&mut self) {
        if self.y > ATLAS * 7 / 8 {
            *self = Atlas::default();
        }
    }

    /// The glyph for `c` at `phys_px` physical pixels, rasterising it on first use.
    pub fn glyph(&mut self, font: Font, c: char, phys_px: f32) -> Glyph {
        let key = (font, c, (phys_px * 4.0).round() as u32);
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let fonts = Fonts::get();
        let g = match fonts.face(font, c) {
            Some((f, _)) => {
                let (m, bmp) = f.rasterize(c, phys_px);
                let (w, h) = (m.width as u32, m.height as u32);
                if w == 0 || h == 0 {
                    Glyph { rect: [0; 4], off_x: 0.0, off_y: 0.0 }
                } else {
                    if self.x + w + 1 >= ATLAS {
                        self.x = 1;
                        self.y += self.row_h + 1;
                        self.row_h = 0;
                    }
                    if self.y + h + 1 >= ATLAS {
                        // full: draw nothing this frame, maintain() resets before the next one
                        return Glyph { rect: [0; 4], off_x: 0.0, off_y: 0.0 };
                    }
                    for row in 0..h {
                        let dst = ((self.y + row) * ATLAS + self.x) as usize;
                        let src = (row * w) as usize;
                        self.pixels[dst..dst + w as usize].copy_from_slice(&bmp[src..src + w as usize]);
                    }
                    let g = Glyph { rect: [self.x, self.y, w, h], off_x: m.xmin as f32, off_y: -(m.ymin as f32 + h as f32) };
                    self.x += w + 1;
                    self.row_h = self.row_h.max(h);
                    self.dirty = true;
                    g
                }
            }
            None => Glyph { rect: [0; 4], off_x: 0.0, off_y: 0.0 },
        };
        self.glyphs.insert(key, g);
        g
    }
}
