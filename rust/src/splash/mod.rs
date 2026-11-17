//! Startup splash for the standalone: a small borderless card with the Jwala logo and the version,
//! shown while the editor decodes its assets and starts the GPU. It goes once the editor presents
//! its first frame (`crate::UI_READY`), or after a timeout so it can never linger; the main window
//! stays hidden until then (`crate::reveal`).
//!
//! The card is drawn here in software; each platform puts it on screen its own way:
//! - Windows: a layered popup on its own thread (`windows.rs`)
//! - macOS: a borderless NSWindow on the main thread, faded from the editor's frame callback,
//!   since AppKit allows no other thread (`macos.rs`)
//! - Linux: an X11 splash window on its own connection and thread; translucent (shadow, rounded
//!   corners, fades) when a compositor is running, a plain card otherwise (`linux.rs`)

use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

const LOGO: &[u8] = include_bytes!("../../../assets/appicon/jwala_512.png");

/// Card size and margin for its shadow, in logical px.
const CARD_W: f32 = 300.0;
const CARD_H: f32 = 280.0;
const RADIUS: f32 = 18.0;
const SHADOW: f32 = 22.0;
const LOGO_SIZE: f32 = 132.0;
const TIMEOUT: Duration = Duration::from_secs(20);
/// Not on macOS: there the main thread, which owns the splash, is busy loading until it fades out.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.22;

/// Shows the splash (call once, early in the standalone's `main`, before the editor opens).
pub fn show() {
    #[cfg(windows)]
    windows::show();
    #[cfg(target_os = "macos")]
    macos::show();
    #[cfg(target_os = "linux")]
    linux::show();
}

/// Called from the editor's frame callback; drives the macOS splash, which lives on the main thread.
pub(crate) fn editor_frame() {
    #[cfg(target_os = "macos")]
    macos::frame();
}

// ---------------------------------------------------------------------------------------------
// drawing: everything is composed in software into premultiplied RGBA floats

pub(crate) struct Card {
    pub w: usize,
    pub h: usize,
    /// Premultiplied RGBA, row-major, top row first.
    px: Vec<[f32; 4]>,
}

impl Card {
    /// Blends a straight-alpha colour over pixel (x, y) with the given coverage.
    fn blend(&mut self, x: usize, y: usize, rgb: [f32; 3], a: f32) {
        if x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let p = &mut self.px[y * self.w + x];
        let k = 1.0 - a;
        p[0] = rgb[0] * a + p[0] * k;
        p[1] = rgb[1] * a + p[1] * k;
        p[2] = rgb[2] * a + p[2] * k;
        p[3] = a + p[3] * k;
    }

    /// Premultiplied BGRA bytes (Windows DIBs, X11 ARGB32 images), all channels scaled by `alpha`.
    #[allow(dead_code)]
    pub fn bgra(&self, alpha: f32, out: &mut Vec<u8>) {
        out.clear();
        for p in &self.px {
            out.extend([q(p[2] * alpha), q(p[1] * alpha), q(p[0] * alpha), q(p[3] * alpha)]);
        }
    }

    /// Premultiplied RGBA bytes (macOS NSBitmapImageRep).
    #[allow(dead_code)]
    pub fn rgba(&self) -> Vec<u8> {
        self.px.iter().flat_map(|p| [q(p[0]), q(p[1]), q(p[2]), q(p[3])]).collect()
    }
}

fn q(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Signed distance from (px, py) to a rounded rectangle centred at (cx, cy) (negative inside).
fn rrect_sdf(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0]
}

/// The faces for the name (the UI's script logo face) and the version line.
fn fonts() -> (Option<fontdue::Font>, Option<fontdue::Font>) {
    use crate::fonts::{load, Face};
    (load(Face::Script).or_else(|| load(Face::Bold)), load(Face::Regular))
}

/// Draws `text` centred horizontally on `cx` with its baseline at `baseline` (physical px).
fn draw_text(c: &mut Card, font: &fontdue::Font, text: &str, px: f32, cx: f32, baseline: f32, color: [f32; 3], tracking: f32) {
    let glyphs: Vec<_> = text.chars().map(|ch| font.rasterize(ch, px)).collect();
    let width: f32 = glyphs.iter().map(|(m, _)| m.advance_width + tracking).sum::<f32>() - tracking;
    let mut pen = (cx - width / 2.0).round();
    for (m, bitmap) in &glyphs {
        let gx = pen as i32 + m.xmin;
        let gy = baseline as i32 - m.height as i32 - m.ymin;
        for row in 0..m.height {
            for col in 0..m.width {
                let cov = bitmap[row * m.width + col] as f32 / 255.0;
                let (x, y) = (gx + col as i32, gy + row as i32);
                if x >= 0 && y >= 0 {
                    c.blend(x as usize, y as usize, color, cov);
                }
            }
        }
        pen += m.advance_width + tracking;
    }
}

/// The splash card at `scale` (physical / logical px). `translucent`: with the drop shadow and
/// rounded corners on a transparent margin; otherwise just the opaque rectangular card (for X11
/// without a compositor, where window transparency isn't available).
pub(crate) fn compose(scale: f32, translucent: bool) -> Option<Card> {
    let s = |v: f32| v * scale;
    let (margin, radius) = if translucent { (SHADOW, RADIUS) } else { (0.0, 0.0) };
    let (w, h) = ((s(CARD_W + 2.0 * margin)).round() as usize, (s(CARD_H + 2.0 * margin)).round() as usize);
    let mut c = Card { w, h, px: vec![[0.0; 4]; w * h] };
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (hw, hh, r) = (s(CARD_W) / 2.0, s(CARD_H) / 2.0, s(radius));
    let card_top = cy - hh;

    let (top, bottom, edge) = (rgb(0x222226), rgb(0x0f0f11), rgb(0xffffff));
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if translucent {
                // soft drop shadow, offset slightly down
                let d_shadow = rrect_sdf(fx, fy - s(5.0), cx, cy, hw, hh, r);
                let shadow = 0.45 * (1.0 - (d_shadow / s(SHADOW)).clamp(0.0, 1.0)).powi(2);
                c.blend(x, y, [0.0; 3], shadow);
            }
            // the card: a dark vertical gradient
            let d = rrect_sdf(fx, fy, cx, cy, hw, hh, r);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                let t = ((fy - card_top) / (2.0 * hh)).clamp(0.0, 1.0);
                let col = [0, 1, 2].map(|i| top[i] + (bottom[i] - top[i]) * t);
                c.blend(x, y, col, cover);
                // a faint 1px rim
                let rim = (1.0 - (d + s(1.0)).abs() / s(1.0)).clamp(0.0, 1.0);
                c.blend(x, y, edge, 0.07 * rim);
            }
        }
    }

    // the logo
    let logo = image::load_from_memory(LOGO).ok()?.to_rgba8();
    let size = s(LOGO_SIZE).round() as u32;
    let logo = image::imageops::resize(&logo, size, size, image::imageops::FilterType::Lanczos3);
    let lx = (cx - size as f32 / 2.0).round() as usize;
    let ly = (card_top + s(34.0)).round() as usize;
    // the icon is an opaque black square: clip it to the rounded white frame drawn inside it
    let (half, inset) = (size as f32 / 2.0, size as f32 * 0.044);
    for (x, y, p) in logo.enumerate_pixels() {
        let d = rrect_sdf(x as f32 + 0.5, y as f32 + 0.5, half, half, half - inset, half - inset, size as f32 * 0.085);
        let a = p[3] as f32 / 255.0 * (0.5 - d).clamp(0.0, 1.0);
        c.blend(lx + x as usize, ly + y as usize, [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0], a);
    }

    // name and version
    let below = card_top + s(34.0) + size as f32;
    let (script, ui) = fonts();
    if let Some(font) = &script {
        draw_text(&mut c, font, "Jwala", s(34.0), cx, below + s(48.0), rgb(0xf2f2f2), 0.0);
    }
    if let Some(font) = ui.as_ref().or(script.as_ref()) {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        draw_text(&mut c, font, &version, s(13.0), cx, below + s(78.0), rgb(0x8a8a90), s(1.0));
    }
    Some(c)
}
