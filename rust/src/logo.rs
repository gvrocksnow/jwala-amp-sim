//! The "Jwala" wordmark, pre-rendered in Segoe Script Bold so it looks the same on every platform
//! (the font itself is Windows-only and can't be shipped). Two images in `assets/`:
//!
//! - `logo_header.png`: the editor header's logo exactly as the UI used to draw it live (GDI+-style
//!   synthetic italic, silver gradient, drop shadow), covering the logical rect `HEADER`
//! - `logo_splash.png`: a plain white mask for the startup splash, tinted when drawn
//!
//! Both are rendered at `BAKE`x. They are regenerated on Windows with
//! `cargo test --lib bake_logo -- --ignored --nocapture`, which prints the constants below.

/// Resolution the images are rendered at (multiple of logical px).
pub(crate) const BAKE: f32 = 4.0;

/// Where `logo_header.png` goes in the header (logical px: x, y, w, h).
pub(crate) const HEADER: (f32, f32, f32, f32) = (14.0, 6.0, 104.0, 52.0);
/// The header logo's baseline (logical y) and the x where text after it may start.
pub(crate) const HEADER_BASELINE: f32 = 39.92;
pub(crate) const HEADER_END: f32 = 105.07;

/// `logo_splash.png`: its size, the pen origin's x offset and the baseline's y offset inside it,
/// and the text's advance width (logical px at the splash's 34 px).
pub(crate) const SPLASH_SIZE: (f32, f32) = (118.0, 59.0);
pub(crate) const SPLASH_ORIGIN: (f32, f32) = (2.0, 40.0);
pub(crate) const SPLASH_ADVANCE: f32 = 111.01;

#[cfg(all(test, windows))]
mod bake {
    use super::*;
    use std::path::PathBuf;

    const TEXT: &str = "Jwala";

    fn font() -> fontdue::Font {
        let dir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
        let bytes = std::fs::read(dir.join("segoescb.ttf")).expect("Segoe Script Bold");
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap()
    }

    fn assets() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("assets")
    }

    /// Pen advances with kerning, as `ui::text::advances` computes them (logical px).
    fn advances(f: &fontdue::Font, px: f32) -> Vec<f32> {
        let mut out: Vec<f32> = Vec::new();
        let mut prev = None;
        for c in TEXT.chars() {
            if let (Some(p), Some(last)) = (prev, out.last_mut()) {
                if let Some(k) = f.horizontal_kern(p, c, px) {
                    *last += k;
                }
            }
            out.push(f.metrics(c, px).advance_width);
            prev = Some(c);
        }
        out
    }

    /// Premultiplied RGBA canvas in baked pixels.
    struct Canvas {
        w: usize,
        h: usize,
        px: Vec<[f32; 4]>,
    }

    impl Canvas {
        fn over(&mut self, x: usize, y: usize, rgb: [f32; 3], a: f32) {
            let p = &mut self.px[y * self.w + x];
            let k = 1.0 - a;
            for i in 0..3 {
                p[i] = rgb[i] * a + p[i] * k;
            }
            p[3] = a + p[3] * k;
        }

        fn save(&self, name: &str) {
            let mut img = image::RgbaImage::new(self.w as u32, self.h as u32);
            for (i, p) in self.px.iter().enumerate() {
                let a = p[3];
                let un = |c: f32| if a > 0.0 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
                let b = |v: f32| (v * 255.0 + 0.5) as u8;
                img.put_pixel((i % self.w) as u32, (i / self.w) as u32, image::Rgba([b(un(p[0])), b(un(p[1])), b(un(p[2])), b(a)]));
            }
            img.save(assets().join(name)).unwrap();
        }
    }

    /// Draws the text like `Painter::glyphs`: each glyph's bitmap placed at the rounded pen
    /// position and sheared about its own bottom edge by `skew`. Coordinates are logical px
    /// relative to the canvas origin; `color(y)` gives the colour at canvas-logical y.
    fn draw(c: &mut Canvas, f: &fontdue::Font, px: f32, x: f32, baseline: f32, skew: f32, color: &dyn Fn(f32) -> ([f32; 3], f32)) {
        let phys = px * BAKE;
        let mut pen = x;
        for (ch, adv) in TEXT.chars().zip(advances(f, px)) {
            let (m, bmp) = f.rasterize(ch, phys);
            let (gw, gh) = (m.width as f32, m.height as f32);
            let gx = (pen * BAKE + m.xmin as f32).round();
            let gy = (baseline * BAKE - (m.ymin as f32 + gh)).round();
            let sample = |lx: f32, ly: f32| -> f32 {
                // bilinear, like the GPU sampling the glyph atlas
                let (fx, fy) = (lx - 0.5, ly - 0.5);
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = (fx - x0, fy - y0);
                let at = |x: f32, y: f32| -> f32 {
                    if x < 0.0 || y < 0.0 || x >= gw || y >= gh {
                        0.0
                    } else {
                        bmp[y as usize * m.width + x as usize] as f32 / 255.0
                    }
                };
                let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
                let bot = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
                top * (1.0 - ty) + bot * ty
            };
            let shear = skew * gh;
            for py in (gy as i64).max(0)..((gy + gh) as i64 + 1).min(c.h as i64) {
                for qx in ((gx - 1.0) as i64).max(0)..((gx + gw + shear + 2.0) as i64).min(c.w as i64) {
                    let ly = py as f32 + 0.5 - gy;
                    // world.x = gx + local.x - skew * (local.y - h)
                    let lx = qx as f32 + 0.5 - gx + skew * (ly - gh);
                    let cov = sample(lx, ly);
                    if cov > 0.0 {
                        let (rgb, a) = color((py as f32 + 0.5) / BAKE);
                        c.over(qx as usize, py as usize, rgb, a * cov);
                    }
                }
            }
            pen += adv;
        }
    }

    #[test]
    #[ignore]
    fn bake_logo() {
        let f = font();

        // header: Painter::logo(Rect(14, 6, 260, 52), 20 pt, h_align 0)
        let (rx, ry, rh) = (14.0f32, 6.0f32, 52.0f32);
        let px = 20.0 * 96.0 / 72.0;
        let lm = f.horizontal_line_metrics(px).unwrap();
        let x = rx + 4.0;
        let base = ry + (rh - (lm.ascent - lm.descent)) / 2.0 + lm.ascent;
        let adv: f32 = advances(&f, px).iter().sum();
        let w = (adv + 16.0).ceil();
        let mut c = Canvas { w: (w * BAKE) as usize, h: (rh * BAKE) as usize, px: vec![[0.0; 4]; (w * BAKE * rh * BAKE) as usize] };
        let skew = 0.18;
        // the shadow, offset by pt / 14 and pt / 11
        draw(&mut c, &f, px, x - rx + 20.0 / 14.0, base - ry + 20.0 / 11.0, skew, &|_| ([0.0; 3], 170.0 / 255.0));
        let (top, bottom) = ([1.0f32, 1.0, 1.0], [150.0 / 255.0, 153.0 / 255.0, 162.0 / 255.0]);
        draw(&mut c, &f, px, x - rx, base - ry, skew, &|y| {
            let t = (y / rh).clamp(0.0, 1.0);
            ([0, 1, 2].map(|i| top[i] + (bottom[i] - top[i]) * t), 1.0)
        });
        c.save("logo_header.png");
        println!("HEADER = ({rx}, {ry}, {w}, {rh}); HEADER_BASELINE = {base:.2}; HEADER_END = {:.2}", x + adv);

        // splash: 34 px, unskewed, white (tinted when drawn)
        let px = 34.0;
        let lm = f.horizontal_line_metrics(px).unwrap();
        let adv: f32 = advances(&f, px).iter().sum();
        let (ox, oy) = (2.0f32, (lm.ascent + 2.0).ceil());
        let (w, h) = ((adv + 6.0).ceil(), (oy - lm.descent + 2.0).ceil());
        let mut c = Canvas { w: (w * BAKE) as usize, h: (h * BAKE) as usize, px: vec![[0.0; 4]; (w * BAKE * h * BAKE) as usize] };
        draw(&mut c, &f, px, ox, oy, 0.0, &|_| ([1.0; 3], 1.0));
        c.save("logo_splash.png");
        println!("SPLASH_SIZE = ({w}, {h}); SPLASH_ORIGIN = ({ox}, {oy}); SPLASH_ADVANCE = {adv:.2}");
    }
}
