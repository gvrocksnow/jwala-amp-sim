//! Windows: the card in a layered (per-pixel alpha) popup on its own thread, topmost and never
//! activated, so it neither steals focus from nor hides behind the window that is loading.

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{compose, FADE_IN, FADE_OUT, TIMEOUT};


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
        // normally a no-op (the editor already revealed its window); after the timeout, or if the
        // splash couldn't be shown, it makes sure a slow start still shows the app
        crate::reveal::reveal();
    });
}

fn run() {
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let Some(card) = compose(scale, true) else { return };
    let (w, h) = (card.w as i32, card.h as i32);
    let mut pixels = Vec::new();
    card.bgra(1.0, &mut pixels);

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
            if closing.is_none() && (crate::UI_READY.load(Ordering::Relaxed) || start.elapsed() > TIMEOUT) {
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

        SelectObject(mem, old);
        DeleteObject(dib);
        DeleteDC(mem);
        ReleaseDC(0, screen);
        DestroyWindow(hwnd);
    }
}
