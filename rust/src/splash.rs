//! Startup splash for the standalone (Windows): a small borderless card with the Jwala logo and the
//! version, shown while the editor decodes its assets and starts the GPU. It runs on its own thread
//! and fades out once the editor presents its first frame (`jwala_plugin::UI_READY`), or after a
//! timeout so it can never linger.

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const LOGO: &[u8] = include_bytes!("../../assets/appicon/jwala_512.png");

/// Card size and margin for its shadow, in logical px.
const CARD_W: f32 = 300.0;
const CARD_H: f32 = 280.0;
const RADIUS: f32 = 18.0;
const SHADOW: f32 = 22.0;
const LOGO_SIZE: f32 = 132.0;
const TIMEOUT: Duration = Duration::from_secs(20);
const FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.22;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}
#[repr(C)]
struct Blend {
    op: u8,
    flags: u8,
    alpha: u8,
    format: u8,
}
#[repr(C)]
struct BitmapInfo {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
    colors: [u32; 1],
}
#[repr(C)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: unsafe extern "system" fn(isize, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_sm: isize,
}
#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    private: u32,
}
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: isize, menu: isize, inst: isize, param: *mut c_void) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn UpdateLayeredWindow(hwnd: isize, dst: isize, pt_dst: *const Point, size: *const Size, src: isize, pt_src: *const Point, key: u32, blend: *const Blend, flags: u32) -> i32;
    fn PeekMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
    fn GetDpiForSystem() -> u32;
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, ini: u32) -> i32;
}
#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(dc: isize) -> isize;
    fn CreateDIBSection(dc: isize, info: *const BitmapInfo, usage: u32, bits: *mut *mut c_void, section: isize, offset: u32) -> isize;
    fn SelectObject(dc: isize, obj: isize) -> isize;
    fn DeleteObject(obj: isize) -> i32;
    fn DeleteDC(dc: isize) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SW_SHOWNOACTIVATE: i32 = 4;
const ULW_ALPHA: u32 = 2;
const PM_REMOVE: u32 = 1;
const SPI_GETWORKAREA: u32 = 0x30;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Shows the splash on a background thread; it closes itself.
pub fn show() {
    std::thread::spawn(|| {
        // a splash that fails to draw is never worth breaking startup over
        let _ = std::panic::catch_unwind(run);
    });
}

fn run() {
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let Some((w, h, pixels)) = compose(scale) else { return };

    unsafe {
        let inst = GetModuleHandleW(std::ptr::null());
        let class = wide("JwalaSplash");
        let wc = WndClassExW {
            size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            wnd_proc: DefWindowProcW,
            cls_extra: 0,
            wnd_extra: 0,
            instance: inst,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
            icon_sm: 0,
        };
        RegisterClassExW(&wc);

        // centred on the primary monitor's work area
        let mut area = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut Rect as *mut c_void, 0);
        let x = area.left + (area.right - area.left - w) / 2;
        let y = area.top + (area.bottom - area.top - h) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("Jwala").as_ptr(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            0,
            0,
            inst,
            std::ptr::null_mut(),
        );
        if hwnd == 0 {
            return;
        }

        // the composed card in a top-down 32-bit premultiplied BGRA DIB
        let screen = GetDC(0);
        let mem = CreateCompatibleDC(screen);
        let info = BitmapInfo {
            size: (std::mem::size_of::<BitmapInfo>() - 4) as u32,
            width: w,
            height: -h,
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_ppm: 0,
            y_ppm: 0,
            clr_used: 0,
            clr_important: 0,
            colors: [0],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem, &info, 0, &mut bits, 0, 0);
        if dib == 0 || bits.is_null() {
            DeleteDC(mem);
            ReleaseDC(0, screen);
            DestroyWindow(hwnd);
            return;
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        let old = SelectObject(mem, dib);

        let pos = Point { x, y };
        let size = Size { cx: w, cy: h };
        let origin = Point { x: 0, y: 0 };
        let present = |alpha: f32| {
            let blend = Blend { op: 0, flags: 0, alpha: (alpha.clamp(0.0, 1.0) * 255.0) as u8, format: 1 /* AC_SRC_ALPHA */ };
            UpdateLayeredWindow(hwnd, screen, &pos, &size, mem, &origin, 0, &blend, ULW_ALPHA);
        };
        present(0.0);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let start = Instant::now();
        let mut closing: Option<Instant> = None;
        let mut msg: Msg = std::mem::zeroed();
        loop {
            while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if closing.is_none() && (jwala_plugin::UI_READY.load(Ordering::Relaxed) || start.elapsed() > TIMEOUT) {
                closing = Some(Instant::now());
            }
            let fade_in = (start.elapsed().as_secs_f32() / FADE_IN).min(1.0);
            let alpha = match closing {
                Some(t) => {
                    let out = t.elapsed().as_secs_f32() / FADE_OUT;
                    if out >= 1.0 {
                        break;
                    }
                    fade_in * (1.0 - out)
                }
                None => fade_in,
            };
            present(alpha);
            std::thread::sleep(Duration::from_millis(15));
        }

        // normally a no-op (the editor already revealed its window); after the timeout it makes sure a
        // slow or stuck start still shows the app rather than leaving it invisible
        jwala_plugin::reveal::reveal();

        SelectObject(mem, old);
        DeleteObject(dib);
        DeleteDC(mem);
        ReleaseDC(0, screen);
        DestroyWindow(hwnd);
    }
}

// ---------------------------------------------------------------------------------------------
// drawing: everything is composed in software into premultiplied RGBA floats, then packed to BGRA

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
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

fn load_font(file: &str) -> Option<fontdue::Font> {
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
    let bytes = std::fs::read(dir.join(file)).ok()?;
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()
}

/// Draws `text` centred horizontally on `cx` with its baseline at `baseline` (physical px).
fn draw_text(c: &mut Canvas, font: &fontdue::Font, text: &str, px: f32, cx: f32, baseline: f32, color: [f32; 3], tracking: f32) {
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

/// The splash card at `scale`: (width, height, premultiplied BGRA bytes).
fn compose(scale: f32) -> Option<(i32, i32, Vec<u8>)> {
    let s = |v: f32| v * scale;
    let (w, h) = ((s(CARD_W + 2.0 * SHADOW)).round() as usize, (s(CARD_H + 2.0 * SHADOW)).round() as usize);
    let mut c = Canvas { w, h, px: vec![[0.0; 4]; w * h] };
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (hw, hh, r) = (s(CARD_W) / 2.0, s(CARD_H) / 2.0, s(RADIUS));
    let card_top = cy - hh;

    let (top, bottom, edge) = (rgb(0x222226), rgb(0x0f0f11), rgb(0xffffff));
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // soft drop shadow, offset slightly down
            let d_shadow = rrect_sdf(fx, fy - s(5.0), cx, cy, hw, hh, r);
            let shadow = 0.45 * (1.0 - (d_shadow / s(SHADOW)).clamp(0.0, 1.0)).powi(2);
            c.blend(x, y, [0.0; 3], shadow);
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
    if let Some(font) = load_font("segoescb.ttf").or_else(|| load_font("segoeuib.ttf")) {
        draw_text(&mut c, &font, "Jwala", s(34.0), cx, below + s(48.0), rgb(0xf2f2f2), 0.0);
    }
    if let Some(font) = load_font("segoeui.ttf") {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        draw_text(&mut c, &font, &version, s(13.0), cx, below + s(78.0), rgb(0x8a8a90), s(1.0));
    }

    // pack premultiplied BGRA for UpdateLayeredWindow
    let mut out = Vec::with_capacity(w * h * 4);
    for p in &c.px {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        out.extend([q(p[2]), q(p[1]), q(p[0]), q(p[3])]);
    }
    Some((w as i32, h as i32, out))
}
