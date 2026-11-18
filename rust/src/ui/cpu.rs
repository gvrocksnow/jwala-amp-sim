//! CPU renderer: the last resort when wgpu finds no adapter at all. macOS VMs without Metal are the
//! typical case: wgpu only runs on Metal there, and Metal has no software device.
//!
//! It draws the same instanced primitives and soft shadows as `shader.wgsl`, with the same maths,
//! into a pixel buffer that softbuffer copies into the window. Like the GPU's software path it
//! draws at 1x (logical pixels) and stretches the frame to the window's physical pixels; the rows
//! are split across all cores.

use super::assets;
use super::gpu::Target;
use super::painter::{Cmd, Inst, Layer, ShadowJob, TexId, K_ELLIPSE, K_GLOW, K_GLYPH, K_GRAD, K_IMAGE, K_LINE, K_RRECT, K_TILE};
use super::text::{Atlas, ATLAS};
use std::collections::HashMap;
use std::num::NonZeroU32;

/// Premultiplied RGBA, 0..1.
type Px = [f32; 4];

/// The window's handles, lent to softbuffer (the window outlives the renderer it owns).
#[derive(Clone, Copy)]
struct Handles(Target);

impl rwh06::HasDisplayHandle for Handles {
    fn display_handle(&self) -> Result<rwh06::DisplayHandle<'_>, rwh06::HandleError> {
        Ok(unsafe { rwh06::DisplayHandle::borrow_raw(self.0 .0) })
    }
}

impl rwh06::HasWindowHandle for Handles {
    fn window_handle(&self) -> Result<rwh06::WindowHandle<'_>, rwh06::HandleError> {
        Ok(unsafe { rwh06::WindowHandle::borrow_raw(self.0 .1) })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Wrap {
    Clamp,
    Repeat,
}

static WHITE: [u8; 4] = [255; 4];

/// An image's premultiplied RGBA8 mip chain (borrowed from the decoded assets).
struct Tex {
    levels: Vec<(usize, usize, &'static [u8])>,
}

impl Tex {
    fn texel(&self, l: usize, x: i64, y: i64, wrap: Wrap) -> Px {
        let (w, h, d) = self.levels[l];
        let (x, y) = match wrap {
            Wrap::Clamp => (x.clamp(0, w as i64 - 1), y.clamp(0, h as i64 - 1)),
            Wrap::Repeat => (x.rem_euclid(w as i64), y.rem_euclid(h as i64)),
        };
        let i = (y as usize * w + x as usize) * 4;
        const K: f32 = 1.0 / 255.0;
        [d[i] as f32 * K, d[i + 1] as f32 * K, d[i + 2] as f32 * K, d[i + 3] as f32 * K]
    }

    fn bilinear(&self, l: usize, u: f32, v: f32, wrap: Wrap) -> Px {
        let (w, h, _) = self.levels[l];
        let (fx, fy) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let top = mix(self.texel(l, x0, y0, wrap), self.texel(l, x0 + 1, y0, wrap), tx);
        let bottom = mix(self.texel(l, x0, y0 + 1, wrap), self.texel(l, x0 + 1, y0 + 1, wrap), tx);
        mix(top, bottom, ty)
    }

    fn trilinear(&self, u: f32, v: f32, lod: f32, wrap: Wrap) -> Px {
        let lod = lod.clamp(0.0, (self.levels.len() - 1) as f32);
        let l = lod.floor() as usize;
        let t = lod - l as f32;
        let a = self.bilinear(l, u, v, wrap);
        if t < 1e-3 || l + 1 >= self.levels.len() {
            return a;
        }
        mix(a, self.bilinear(l + 1, u, v, wrap), t)
    }
}

fn mix(a: Px, b: Px, t: f32) -> Px {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

fn sd_rrect(p: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let q = (p.0.abs() - half.0 + r, p.1.abs() - half.1 + r);
    let (mx, my) = (q.0.max(0.0), q.1.max(0.0));
    (mx * mx + my * my).sqrt() + q.0.max(q.1).min(0.0) - r
}

fn sd_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let pa = (p.0 - a.0, p.1 - a.1);
    let ba = (b.0 - a.0, b.1 - a.1);
    let h = ((pa.0 * ba.0 + pa.1 * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1).max(1e-6)).clamp(0.0, 1.0);
    let (dx, dy) = (pa.0 - ba.0 * h, pa.1 - ba.1 * h);
    (dx * dx + dy * dy).sqrt()
}

/// A cached soft shadow: each light term's blurred occluder alpha, and the receiver's alpha.
struct Mask {
    w: usize,
    h: usize,
    out: [Vec<f32>; 3],
    rec: Option<Vec<f32>>,
    /// Per term: offset in uv, strength.
    terms: [[f32; 3]; 3],
}

/// Bilinear, clamp-to-edge lookup in a single-channel image.
fn sample_a(buf: &[f32], w: usize, h: usize, u: f32, v: f32) -> f32 {
    let (fx, fy) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |x: f32, y: f32| buf[(y.clamp(0.0, h as f32 - 1.0) as usize) * w + x.clamp(0.0, w as f32 - 1.0) as usize];
    let top = at(x0, y0) + (at(x0 + 1.0, y0) - at(x0, y0)) * tx;
    let bottom = at(x0, y0 + 1.0) + (at(x0 + 1.0, y0 + 1.0) - at(x0, y0 + 1.0)) * tx;
    top + (bottom - top) * ty
}

/// Separable Gaussian, clamp-to-edge (`fs_blur`).
fn blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let s = sigma.max(0.01);
    let r = (s * 3.0).ceil() as i64;
    let mut k: Vec<f32> = (0..=r).map(|i| (-(i * i) as f32 / (2.0 * s * s)).exp()).collect();
    let sum: f32 = k[0] + 2.0 * k[1..].iter().sum::<f32>();
    k.iter_mut().for_each(|x| *x /= sum);
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut dst = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for i in -r..=r {
                    let (sx, sy) = if horizontal { ((x as i64 + i).clamp(0, w as i64 - 1) as usize, y) } else { (x, (y as i64 + i).clamp(0, h as i64 - 1) as usize) };
                    acc += src[sy * w + sx] * k[i.unsigned_abs() as usize];
                }
                dst[y * w + x] = acc;
            }
        }
        dst
    };
    pass(&pass(src, true), false)
}

/// Everything a band of rows needs to draw a layer.
struct Ctx<'a> {
    textures: &'a [Tex],
    glyphs: &'a [u8],
    slots: &'a [Option<Mask>],
    /// The UI scale (`Globals.scale`): antialiasing is about one pixel of it.
    aa: f32,
}

/// One draw target, or a horizontal band of it: rows `y0..` of an image `w` x `full_h` pixels,
/// `ppx` x `ppy` pixels per logical pixel.
struct Band<'a> {
    px: &'a mut [Px],
    w: usize,
    y0: usize,
    full_h: usize,
    ppx: f32,
    ppy: f32,
}

impl Ctx<'_> {
    fn draw(&self, layer: &Layer, b: &mut Band) {
        for c in &layer.cmds {
            match c {
                Cmd::Quads { tex, kind, start, end } => {
                    let tex = self.textures.get(tex.0).unwrap_or(&self.textures[0]);
                    for i in &layer.inst[*start as usize..*end as usize] {
                        self.quad(i, *kind as f32, tex, b);
                    }
                }
                Cmd::Shadow(slot) => {
                    if let Some(Some(m)) = self.slots.get(*slot) {
                        Self::shadow(m, b);
                    }
                }
            }
        }
    }

    /// Composites a cached shadow (`fs_shadow`): premultiplied black over the target.
    fn shadow(m: &Mask, b: &mut Band) {
        let rows = b.px.len() / b.w;
        for row in 0..rows {
            let v = ((b.y0 + row) as f32 + 0.5) / b.full_h as f32;
            for x in 0..b.w {
                let u = (x as f32 + 0.5) / b.w as f32;
                let mut light = 1.0;
                for (t, out) in m.terms.iter().zip(&m.out) {
                    if t[2] != 0.0 {
                        light *= 1.0 - t[2] * sample_a(out, m.w, m.h, u - t[0], v - t[1]);
                    }
                }
                let mut a = 1.0 - light;
                if let Some(rec) = &m.rec {
                    a *= sample_a(rec, m.w, m.h, u, v);
                }
                if a > 0.0 {
                    let d = &mut b.px[row * b.w + x];
                    for c in d.iter_mut() {
                        *c *= 1.0 - a;
                    }
                }
            }
        }
    }

    /// One instanced quad (`vs_quad` + `fs_quad`).
    fn quad(&self, i: &Inst, k: f32, tex: &Tex, b: &mut Band) {
        let [rx, ry, rw, rh] = i.rect;
        let pad = if (k == K_GLYPH || k == K_IMAGE || k == K_TILE) && i.p[1] == 0.0 { 0.0 } else { 1.5 };
        // italic glyphs are sheared: world.x = rect.x + local.x - skew * (local.y - h)
        let skew = if k == K_GLYPH { i.p[3] } else { 0.0 };
        let (s0, s1) = (skew * (rh + pad), -skew * pad);
        let c = i.clip;
        let (wx0, wx1) = ((rx - pad + s0.min(s1)).max(c[0]), (rx + rw + pad + s0.max(s1)).min(c[2]));
        let (wy0, wy1) = ((ry - pad).max(c[1]), (ry + rh + pad).min(c[3]));
        if !(wx1 >= wx0 && wy1 >= wy0) {
            return;
        }
        let rows = b.px.len() / b.w;
        // pixels whose centres fall inside
        let px0 = (wx0 * b.ppx - 0.5).ceil().max(0.0);
        let px1 = (wx1 * b.ppx - 0.5).floor().min(b.w as f32 - 1.0);
        let py0 = (wy0 * b.ppy - 0.5).ceil().max(b.y0 as f32);
        let py1 = (wy1 * b.ppy - 0.5).floor().min((b.y0 + rows) as f32 - 1.0);
        if px1 < px0 || py1 < py0 {
            return;
        }

        let half = (rw * 0.5, rh * 0.5);
        let aa = 0.75 / self.aa;
        let cover = |d: f32| (0.5 - d / (2.0 * aa)).clamp(0.0, 1.0);
        let stroke_cover = |d: f32| if i.p[2] > 0.0 { cover((d + i.p[2] * 0.5).abs() - i.p[2] * 0.5) } else { cover(d) };
        let (uv0, duv) = ((i.uv[0], i.uv[1]), (i.uv[2] - i.uv[0], i.uv[3] - i.uv[1]));
        let (inv_w, inv_h) = (1.0 / rw.max(1e-4), 1.0 / rh.max(1e-4));
        // images: the texture footprint of one pixel is the same everywhere in the quad
        let (wrap, lod) = {
            let (tw, th, _) = tex.levels[0];
            let fp = (duv.0 * inv_w / b.ppx * tw as f32).abs().max((duv.1 * inv_h / b.ppy * th as f32).abs());
            // `sample_img` sharpens minified images with 4 taps one mip finer; one tap half a
            // level finer is a cheaper middle ground
            (if k == K_TILE { Wrap::Repeat } else { Wrap::Clamp }, if fp <= 1.0 { 0.0 } else { fp.log2() - 0.5 })
        };
        let radius = half.0.min(half.1);
        let er = (half.0.max(1e-3), half.1.max(1e-3));

        for py in py0 as usize..=py1 as usize {
            let wy = (py as f32 + 0.5) / b.ppy;
            let ly = wy - ry;
            let row = (py - b.y0) * b.w;
            for px in px0 as usize..=px1 as usize {
                let wx = (px as f32 + 0.5) / b.ppx;
                let lx = wx - rx + skew * (ly - rh);
                if lx < -pad || lx > rw + pad {
                    continue;
                }
                let pc = (lx - half.0, ly - half.1);
                let uv = (uv0.0 + duv.0 * lx * inv_w, uv0.1 + duv.1 * ly * inv_h);
                let mut col = i.color;
                let a;
                let src: Px = if k == K_IMAGE || k == K_TILE {
                    let s = tex.trilinear(uv.0, uv.1, lod, wrap);
                    let m = if i.p[1] <= 0.0 { 1.0 } else { cover(sd_rrect(pc, half, i.p[1].min(radius))) };
                    [s[0] * col[0] * m, s[1] * col[1] * m, s[2] * col[2] * m, s[3] * col[3] * m]
                } else {
                    if k == K_RRECT || k == K_GRAD {
                        a = stroke_cover(sd_rrect(pc, half, i.p[1].min(radius)));
                        if k == K_GRAD {
                            col = mix(i.color, i.color2, ((wy - i.q[0]) / (i.q[1] - i.q[0]).max(1e-4)).clamp(0.0, 1.0));
                        }
                    } else if k == K_GLYPH {
                        a = glyph(self.glyphs, uv.0, uv.1);
                        if i.q[1] > i.q[0] {
                            col = mix(i.color, i.color2, ((wy - i.q[0]) / (i.q[1] - i.q[0])).clamp(0.0, 1.0));
                        }
                    } else if k == K_ELLIPSE {
                        let kk = ((pc.0 / er.0).powi(2) + (pc.1 / er.1).powi(2)).sqrt();
                        a = stroke_cover((kk - 1.0) * er.0.min(er.1));
                    } else if k == K_GLOW {
                        let kk = ((pc.0 / er.0).powi(2) + (pc.1 / er.1).powi(2)).sqrt();
                        col = mix(i.color, i.color2, ((kk - i.p[3]) / (1.0 - i.p[3]).max(1e-3)).clamp(0.0, 1.0));
                        a = if kk > 1.0 { 0.0 } else { 1.0 };
                    } else if k == K_LINE {
                        a = cover(sd_segment((wx, wy), (i.q[0], i.q[1]), (i.q[2], i.q[3])) - i.p[2] * 0.5);
                    } else {
                        a = 1.0;
                    }
                    let alpha = col[3] * a;
                    [col[0] * alpha, col[1] * alpha, col[2] * alpha, alpha]
                };
                if src[3] <= 0.0 && src[0] <= 0.0 && src[1] <= 0.0 && src[2] <= 0.0 {
                    continue;
                }
                // premultiplied "over"
                let d = &mut b.px[row + px];
                let ia = 1.0 - src[3];
                *d = [src[0] + d[0] * ia, src[1] + d[1] * ia, src[2] + d[2] * ia, src[3] + d[3] * ia];
            }
        }
    }
}

/// Bilinear lookup in the glyph atlas (R8, clamp).
fn glyph(atlas: &[u8], u: f32, v: f32) -> f32 {
    let n = ATLAS as f32;
    let (fx, fy) = (u * n - 0.5, v * n - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |x: f32, y: f32| atlas[(y.clamp(0.0, n - 1.0) as usize) * ATLAS as usize + x.clamp(0.0, n - 1.0) as usize] as f32 / 255.0;
    let top = at(x0, y0) + (at(x0 + 1.0, y0) - at(x0, y0)) * tx;
    let bottom = at(x0, y0 + 1.0) + (at(x0 + 1.0, y0 + 1.0) - at(x0, y0 + 1.0)) * tx;
    top + (bottom - top) * ty
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 16)
}

pub struct Renderer {
    surface: softbuffer::Surface<Handles, Handles>,
    _context: softbuffer::Context<Handles>,
    textures: Vec<Tex>,
    names: HashMap<&'static str, TexId>,
    pub tex_sizes: Vec<(u32, u32)>,
    slots: Vec<Option<Mask>>,
    /// Layout key each shadow slot was rendered for (shared with the painter).
    pub shadow_keys: Vec<u64>,
    /// The window in physical pixels, and the frame drawn at 1x.
    phys: (u32, u32),
    size: (usize, usize),
    frame: Vec<Px>,
}

impl Renderer {
    pub fn new(target: Target, phys: (u32, u32), scale: f32) -> Result<Renderer, String> {
        let handles = Handles(target);
        let context = softbuffer::Context::new(handles).map_err(|e| e.to_string())?;
        let surface = softbuffer::Surface::new(&context, handles).map_err(|e| e.to_string())?;
        let mut r = Renderer {
            surface,
            _context: context,
            textures: Vec::new(),
            names: HashMap::new(),
            tex_sizes: Vec::new(),
            slots: Vec::new(),
            shadow_keys: Vec::new(),
            phys: (0, 0),
            size: (0, 0),
            frame: Vec::new(),
        };
        r.textures.push(Tex { levels: vec![(1, 1, &WHITE[..])] }); // TexId::WHITE
        r.tex_sizes.push((1, 1));
        for img in assets::images() {
            let mut levels = Vec::new();
            let (mut w, mut h) = (img.width as usize, img.height as usize);
            for m in &img.mips {
                levels.push((w, h, &m[..]));
                (w, h) = ((w / 2).max(1), (h / 2).max(1));
            }
            r.names.insert(img.name, TexId(r.textures.len()));
            r.textures.push(Tex { levels });
            r.tex_sizes.push((img.width, img.height));
        }
        r.resize(phys, scale);
        eprintln!("Jwala: renderer CPU ({} threads), {}x{} px drawn at {}x{}", threads(), r.phys.0, r.phys.1, r.size.0, r.size.1);
        Ok(r)
    }

    pub fn tex(&self, name: &str) -> TexId {
        self.names.get(name).copied().unwrap_or(TexId::WHITE)
    }

    pub fn logical_size(&self) -> (f32, f32) {
        (self.size.0 as f32, self.size.1 as f32)
    }

    /// Always 1: the frame is drawn in logical pixels and stretched to the window.
    pub fn scale(&self) -> f32 {
        1.0
    }

    pub fn resize(&mut self, phys: (u32, u32), scale: f32) {
        let phys = (phys.0.max(1), phys.1.max(1));
        if phys != self.phys {
            self.phys = phys;
            if let Err(e) = self.surface.resize(NonZeroU32::new(phys.0).unwrap(), NonZeroU32::new(phys.1).unwrap()) {
                eprintln!("Jwala: CPU renderer resize failed: {e}");
            }
        }
        let scale = scale.max(0.25);
        let size = (((phys.0 as f32 / scale).round() as usize).max(1), ((phys.1 as f32 / scale).round() as usize).max(1));
        if size != self.size {
            self.size = size;
            self.frame = vec![[0.0; 4]; size.0 * size.1];
            self.slots.clear();
            self.shadow_keys.clear();
        }
    }

    /// Renders the occluder and receiver masks of one shadow and blurs them (at 1/8 size, as
    /// the GPU's software path does).
    fn mask(ctx: &Ctx, job: &ShadowJob, size: (usize, usize)) -> Mask {
        const DIV: usize = 8;
        let (w, h) = ((size.0 / DIV).max(1), (size.1 / DIV).max(1));
        let (ppx, ppy) = (w as f32 / size.0 as f32, h as f32 / size.1 as f32);
        let alpha = |layer: &Layer| -> Vec<f32> {
            let mut px = vec![[0.0; 4]; w * h];
            ctx.draw(layer, &mut Band { px: &mut px, w, y0: 0, full_h: h, ppx, ppy });
            px.iter().map(|p| p[3]).collect()
        };
        let occ = alpha(&job.occluders);
        let sigma_scale = 1.0 / DIV as f32;
        let out = [0, 1, 2].map(|k| if job.terms[k].strength != 0.0 { blur(&occ, w, h, job.terms[k].blur * sigma_scale) } else { vec![0.0; w * h] });
        let terms = [0, 1, 2].map(|k| {
            let t = job.terms[k];
            [t.dx / size.0 as f32, t.dy / size.1 as f32, t.strength]
        });
        Mask { w, h, out, rec: job.receiver.as_ref().map(alpha), terms }
    }

    /// Renders and presents one frame. Returns false if it couldn't be shown; the caller retries.
    pub fn render(&mut self, layer: &Layer, jobs: &[ShadowJob], atlas: &mut Atlas) -> bool {
        // the atlas is read straight from memory
        atlas.dirty = false;
        let (w, h) = self.size;

        let masks: Vec<(usize, Mask)> = {
            let ctx = Ctx { textures: &self.textures, glyphs: &atlas.pixels, slots: &self.slots, aa: 1.0 };
            jobs.iter().map(|j| (j.slot, Self::mask(&ctx, j, self.size))).collect()
        };
        for (slot, m) in masks {
            if self.slots.len() <= slot {
                self.slots.resize_with(slot + 1, || None);
            }
            self.slots[slot] = Some(m);
        }

        let mut frame = std::mem::take(&mut self.frame);
        frame.fill([0.0, 0.0, 0.0, 1.0]);
        let rows = h.div_ceil(threads()).max(1);
        {
            let ctx = Ctx { textures: &self.textures, glyphs: &atlas.pixels, slots: &self.slots, aa: 1.0 };
            let ctx = &ctx;
            std::thread::scope(|s| {
                for (n, px) in frame.chunks_mut(rows * w).enumerate() {
                    s.spawn(move || ctx.draw(layer, &mut Band { px, w, y0: n * rows, full_h: h, ppx: 1.0, ppy: 1.0 }));
                }
            });
        }

        let (pw, ph) = (self.phys.0 as usize, self.phys.1 as usize);
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                self.frame = frame;
                eprintln!("Jwala: CPU renderer has no buffer: {e}");
                return false;
            }
        };
        let pack = |p: Px| -> u32 {
            let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2])
        };
        // stretch the 1x frame to the window (bilinear)
        let src = &frame;
        let out_rows = ph.div_ceil(threads()).max(1);
        std::thread::scope(|s| {
            for (n, out) in buf.chunks_mut(out_rows * pw).enumerate() {
                s.spawn(move || {
                    for (r, line) in out.chunks_mut(pw).enumerate() {
                        let y = n * out_rows + r;
                        if (pw, ph) == (w, h) {
                            for (x, o) in line.iter_mut().enumerate() {
                                *o = pack(src[y * w + x]);
                            }
                            continue;
                        }
                        let fy = ((y as f32 + 0.5) * h as f32 / ph as f32 - 0.5).clamp(0.0, (h - 1) as f32);
                        let (y0, ty) = (fy as usize, fy.fract());
                        let y1 = (y0 + 1).min(h - 1);
                        for (x, o) in line.iter_mut().enumerate() {
                            let fx = ((x as f32 + 0.5) * w as f32 / pw as f32 - 0.5).clamp(0.0, (w - 1) as f32);
                            let (x0, tx) = (fx as usize, fx.fract());
                            let x1 = (x0 + 1).min(w - 1);
                            let top = mix(src[y0 * w + x0], src[y0 * w + x1], tx);
                            let bottom = mix(src[y1 * w + x0], src[y1 * w + x1], tx);
                            *o = pack(mix(top, bottom, ty));
                        }
                    }
                });
            }
        });
        let shown = buf.present();
        self.frame = frame;
        if let Err(e) = shown {
            eprintln!("Jwala: CPU renderer couldn't present: {e}");
            return false;
        }
        if !crate::UI_READY.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("Jwala: first frame presented ({} draws, CPU)", layer.inst.len());
        }
        true
    }
}
