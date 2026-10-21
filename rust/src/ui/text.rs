//! Text: the Windows fonts the C# UI used, rasterised with fontdue into a glyph atlas texture.
//! Sizes are in points like GDI+ (1 pt = 96/72 logical px); glyphs are rasterised at physical size.

use fontdue::{Font as FdFont, FontSettings};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Font {
    Regular,
    Bold,
    Semibold,
    Light,
    /// Segoe Script Bold, for the logo.
    Script,
    ArialBold,
}

const FONT_FILES: [(Font, &str); 6] = [
    (Font::Regular, "segoeui.ttf"),
    (Font::Bold, "segoeuib.ttf"),
    (Font::Semibold, "seguisb.ttf"),
    (Font::Light, "segoeuil.ttf"),
    (Font::Script, "segoescb.ttf"),
    (Font::ArialBold, "arialbd.ttf"),
];
/// Fallback for symbols (◀ ▶ ♩ ♭ ♯ ⚙ …).
const SYMBOL_FILE: &str = "seguisym.ttf";

pub struct Fonts {
    fonts: HashMap<Font, FdFont>,
    symbol: Option<FdFont>,
}

impl Fonts {
    fn load() -> Fonts {
        let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
        let read = |f: &str| std::fs::read(dir.join(f)).ok().and_then(|b| FdFont::from_bytes(b, FontSettings::default()).ok());
        let mut fonts = HashMap::new();
        // anything missing falls back to the regular face (or the first face found)
        let mut fallback: Option<Vec<u8>> = None;
        for (id, file) in FONT_FILES {
            if let Some(bytes) = std::fs::read(dir.join(file)).ok() {
                if fallback.is_none() {
                    fallback = Some(bytes.clone());
                }
                if let Ok(f) = FdFont::from_bytes(bytes, FontSettings::default()) {
                    fonts.insert(id, f);
                }
            }
        }
        for (id, _) in FONT_FILES {
            if !fonts.contains_key(&id) {
                if let Some(b) = fallback.clone() {
                    if let Ok(f) = FdFont::from_bytes(b, FontSettings::default()) {
                        fonts.insert(id, f);
                    }
                }
            }
        }
        Fonts { fonts, symbol: read(SYMBOL_FILE) }
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
