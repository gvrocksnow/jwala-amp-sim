//! Keeps the standalone's main window invisible until the editor has drawn its first frame, so the
//! user sees the splash and then the finished UI, never a blank window.
//!
//! - macOS: `hide_window` (from the editor's setup) sets the NSWindow's alpha to 0. baseview has
//!   just ordered it front, but the main thread is busy with that setup until the first frame, so
//!   nothing is drawn in between.
//! - Linux: not hidden. Setting `_NET_WM_WINDOW_OPACITY` to 0 and removing it after the first frame
//!   left the window see-through on some compositors (`hide_by_opacity`, kept but unused).
//!   `hide_window` instead makes baseview's 32-bit ARGB windows opaque (background + opaque region).
//!   Unmapping it instead isn't an option: presenting to an unmapped window can stall the GPU driver.
//! - Windows: baseview creates the top-level window already visible (`WS_VISIBLE` is hard-coded), so a
//! thread-local CBT hook on the GUI thread spots its creation, and a CALLWNDPROCRET hook DWM-cloaks it
//! right after WM_CREATE. A cloaked window
//! is still "visible" to user32 and keeps rendering normally; it's just not composed on screen.
//! The editor uncloaks it after the first present (`reveal`).
//!
//! The main window also reopens where it was last closed, pulled fully onto the screen if part of
//! it would be off it (`window-pos.txt`). Windows: moved in `reveal` (still cloaked), saved by a
//! WH_CALLWNDPROC hook on move/close. macOS and Linux: moved in `hide_window`, saved from the
//! editor's WillClose (`save_position`).

/// Where the standalone's main window was when last closed: the top-left of its frame, in screen
/// coordinates with y down (one file for every platform).
#[allow(dead_code)]
fn load_pos() -> Option<(i32, i32)> {
    let t = std::fs::read_to_string(crate::presets::app_dir().join("window-pos.txt")).ok()?;
    let mut it = t.split_whitespace().map(|v| v.parse::<i32>().ok());
    Some((it.next()??, it.next()??))
}

#[allow(dead_code)]
fn save_pos(x: i32, y: i32) {
    let dir = crate::presets::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("window-pos.txt"), format!("{x} {y}"));
}

/// Where to put a `w`x`h` frame wanted at (`x`, `y`) so it is fully inside one of `areas`
/// ((left, top, width, height), y down): the area it overlaps most, else the nearest. A frame
/// bigger than the area keeps its top-left in the area (so the title bar stays reachable).
#[allow(dead_code)]
fn fit_into(x: i32, y: i32, w: i32, h: i32, areas: &[(i32, i32, i32, i32)]) -> (i32, i32) {
    let overlap = |a: &(i32, i32, i32, i32)| {
        let ox = ((x + w).min(a.0 + a.2) - x.max(a.0)).max(0) as i64;
        let oy = ((y + h).min(a.1 + a.3) - y.max(a.1)).max(0) as i64;
        ox * oy
    };
    let dist = |a: &(i32, i32, i32, i32)| {
        let dx = (x as i64 + w as i64 / 2) - (a.0 as i64 + a.2 as i64 / 2);
        let dy = (y as i64 + h as i64 / 2) - (a.1 as i64 + a.3 as i64 / 2);
        dx * dx + dy * dy
    };
    let best = areas.iter().max_by(|a, b| overlap(a).cmp(&overlap(b)).then(dist(b).cmp(&dist(a))));
    let Some(a) = best else { return (x, y) };
    let fit = |pos: i32, size: i32, lo: i32, len: i32| if size >= len { lo } else { pos.clamp(lo, lo + len - size) };
    (fit(x, w, a.0, a.2), fit(y, h, a.1, a.3))
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};

    #[repr(C)]
    struct CreateStruct {
        create_params: *mut c_void,
        instance: isize,
        menu: isize,
        parent: isize,
        cy: i32,
        cx: i32,
        y: i32,
        x: i32,
        style: i32,
        name: *const u16,
        class: *const u16,
        ex_style: u32,
    }
    #[repr(C)]
    struct CwpRet {
        result: isize,
        lparam: isize,
        wparam: usize,
        message: u32,
        hwnd: isize,
    }
    #[repr(C)]
    struct CbtCreateWnd {
        cs: *mut CreateStruct,
        insert_after: isize,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowsHookExW(id: i32, proc_: unsafe extern "system" fn(i32, usize, isize) -> isize, module: isize, thread: u32) -> isize;
        fn UnhookWindowsHookEx(hook: isize) -> i32;
        fn CallNextHookEx(hook: isize, code: i32, wparam: usize, lparam: isize) -> isize;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn MonitorFromRect(rect: *const Rect, flags: u32) -> isize;
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
        fn IsZoomed(hwnd: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const c_void, size: u32) -> i32;
        fn DwmGetWindowAttribute(hwnd: isize, attr: u32, value: *mut c_void, size: u32) -> i32;
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }
    #[repr(C)]
    struct CwpStruct {
        lparam: isize,
        wparam: usize,
        message: u32,
        hwnd: isize,
    }

    const WH_CBT: i32 = 5;
    const WH_CALLWNDPROCRET: i32 = 12;
    const HCBT_CREATEWND: i32 = 3;
    const WM_CREATE: u32 = 1;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const DWMWA_CLOAK: u32 = 13;
    const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
    const WH_CALLWNDPROC: i32 = 4;
    const WM_DESTROY: u32 = 2;
    const WM_CLOSE: u32 = 0x10;
    const WM_EXITSIZEMOVE: u32 = 0x232;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SWP_NOSIZE: u32 = 1;
    const SWP_NOZORDER: u32 = 4;
    const SWP_NOACTIVATE: u32 = 0x10;
    const SWP_ASYNCWINDOWPOS: u32 = 0x4000;

    static MAIN: AtomicIsize = AtomicIsize::new(0);
    static MOVE_HOOK: AtomicIsize = AtomicIsize::new(0);

    /// The window rectangle and the part of it that is actually drawn (Windows 10+ adds invisible
    /// resize borders around the frame, which may hang off the screen).
    fn rects(hwnd: isize) -> Option<(Rect, Rect)> {
        let mut outer = Rect::default();
        if unsafe { GetWindowRect(hwnd, &mut outer) } == 0 {
            return None;
        }
        let mut visible = outer;
        let ok = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut visible as *mut Rect as *mut c_void, 16) } >= 0;
        Some((outer, if ok { visible } else { outer }))
    }

    /// Remembers where the main window's visible frame is (not while minimized or maximized).
    fn save_position(hwnd: isize) {
        if unsafe { IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 } {
            return;
        }
        if let Some((_, v)) = rects(hwnd) {
            super::save_pos(v.left, v.top);
        }
    }

    /// Moves the main window to its saved spot, pulled inside the work area of the monitor nearest
    /// to it if any part would be off screen (top-left wins when the window is bigger than the screen).
    /// With nothing saved yet (first start), centres it on the monitor it was created on (baseview
    /// creates it at 0,0: the primary monitor).
    fn restore_position(hwnd: isize) {
        let saved = super::load_pos();
        let Some((outer, vis)) = rects(hwnd) else { return };
        let (w, h) = (vis.right - vis.left, vis.bottom - vis.top);
        let (x, y) = saved.unwrap_or((vis.left, vis.top));
        let want = Rect { left: x, top: y, right: x + w, bottom: y + h };
        let mut info = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0 };
        if unsafe { GetMonitorInfoW(MonitorFromRect(&want, MONITOR_DEFAULTTONEAREST), &mut info) } == 0 {
            return;
        }
        let wa = info.work;
        let (x, y) = if saved.is_some() { (x, y) } else { (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2) };        let fit = |pos: i32, size: i32, lo: i32, hi: i32| if size >= hi - lo { lo } else { pos.clamp(lo, hi - size) };
        let (x, y) = (fit(x, w, wa.left, wa.right), fit(y, h, wa.top, wa.bottom));
        if (x, y) == (vis.left, vis.top) {
            return;
        }
        // the window rectangle sits this far outside the visible frame
        let (dx, dy) = (vis.left - outer.left, vis.top - outer.top);
        // from the splash's thread, don't wait on a busy GUI thread
        let other_thread = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) != GetCurrentThreadId() };
        let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | if other_thread { SWP_ASYNCWINDOWPOS } else { 0 };
        unsafe { SetWindowPos(hwnd, 0, x - dx, y - dy, 0, 0, flags) };
    }

    unsafe extern "system" fn move_hook(code: i32, wparam: usize, lparam: isize) -> isize {
        if code >= 0 {
            let m = &*(lparam as *const CwpStruct);
            if m.hwnd != 0 && m.hwnd == MAIN.load(Ordering::Relaxed) {
                match m.message {
                    WM_EXITSIZEMOVE | WM_CLOSE => save_position(m.hwnd),
                    WM_DESTROY => {
                        save_position(m.hwnd);
                        MAIN.store(0, Ordering::Relaxed);
                        let h = MOVE_HOOK.swap(0, Ordering::Relaxed);
                        if h != 0 {
                            UnhookWindowsHookEx(h);
                        }
                    }
                    _ => {}
                }
            }
        }
        CallNextHookEx(0, code, wparam, lparam)
    }

    static HOOK: AtomicIsize = AtomicIsize::new(0);
    static CLOAKED: AtomicIsize = AtomicIsize::new(0);

    static RET_HOOK: AtomicIsize = AtomicIsize::new(0);
    static TARGET: AtomicIsize = AtomicIsize::new(0);

    fn cloak(hwnd: isize, on: bool) -> bool {
        let v: i32 = on as i32;
        unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const i32 as *const c_void, 4) >= 0 }
    }

    unsafe extern "system" fn ret_hook(code: i32, wparam: usize, lparam: isize) -> isize {
        if code >= 0 {
            let m = &*(lparam as *const CwpRet);
            if m.message == WM_CREATE && m.hwnd == TARGET.load(Ordering::Relaxed) {
                if cloak(m.hwnd, true) {
                    CLOAKED.store(m.hwnd, Ordering::Relaxed);
                }
                MAIN.store(m.hwnd, Ordering::Relaxed);
                MOVE_HOOK.store(SetWindowsHookExW(WH_CALLWNDPROC, move_hook, 0, GetCurrentThreadId()), Ordering::Relaxed);
                let h = RET_HOOK.swap(0, Ordering::Relaxed);
                if h != 0 {
                    UnhookWindowsHookEx(h);
                }
            }
        }
        CallNextHookEx(0, code, wparam, lparam)
    }

    unsafe extern "system" fn hook(code: i32, wparam: usize, lparam: isize) -> isize {
        if code == HCBT_CREATEWND {
            let cs = &*(*(lparam as *const CbtCreateWnd)).cs;
            let style = cs.style as u32;
            // the first visible captioned top-level window on this thread: baseview's main window
            if style & WS_CHILD == 0 && style & WS_VISIBLE != 0 && style & WS_CAPTION == WS_CAPTION {
                // DWM can't cloak it yet (E_HANDLE): do that once WM_CREATE has run, which is still
                // before CreateWindowEx shows it
                TARGET.store(wparam as isize, Ordering::Relaxed);
                RET_HOOK.store(SetWindowsHookExW(WH_CALLWNDPROCRET, ret_hook, 0, GetCurrentThreadId()), Ordering::Relaxed);
                let h = HOOK.swap(0, Ordering::Relaxed);
                if h != 0 {
                    UnhookWindowsHookEx(h);
                }
            }
        }
        CallNextHookEx(0, code, wparam, lparam)
    }

    /// Call on the thread that will create the window, before it does.
    pub fn hide_next_main_window() {
        unsafe {
            let h = SetWindowsHookExW(WH_CBT, hook, 0, GetCurrentThreadId());
            HOOK.store(h, Ordering::Relaxed);
        }
    }

    /// Uncloaks the main window (no-op if it wasn't cloaked or is already shown).
    pub fn reveal() {
        let hwnd = CLOAKED.swap(0, Ordering::Relaxed);
        if hwnd != 0 {
            restore_position(hwnd);
            cloak(hwnd, false);
            unsafe { SetForegroundWindow(hwnd) };
        }
    }
}

#[cfg(windows)]
pub use win::{hide_next_main_window, reveal};

/// Windows saves the main window's position from its own hook.
#[cfg(windows)]
pub(crate) fn save_position() {}

/// Hides the standalone's top-level window holding the editor view `handle` (macOS, Linux; on
/// Windows it is already hidden from creation).
#[cfg(windows)]
pub(crate) fn hide_window(_handle: raw_window_handle::RawWindowHandle) {}

#[cfg(target_os = "macos")]
mod mac {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::RawWindowHandle;
    use std::sync::atomic::{AtomicPtr, Ordering};

    static HIDDEN: AtomicPtr<Object> = AtomicPtr::new(std::ptr::null_mut());
    /// The standalone's main window (for `save_position`).
    static MAIN: AtomicPtr<Object> = AtomicPtr::new(std::ptr::null_mut());

    /// The screens' usable areas (minus menu bar and Dock) with y flipped to point down.
    unsafe fn screen_areas() -> Vec<(i32, i32, i32, i32)> {
        use cocoa::appkit::NSScreen;
        use cocoa::base::nil;
        use cocoa::foundation::NSArray;
        let screens = NSScreen::screens(nil);
        if screens.is_null() {
            return Vec::new();
        }
        (0..screens.count())
            .map(|i| {
                let f = NSScreen::visibleFrame(screens.objectAtIndex(i));
                (f.origin.x as i32, -((f.origin.y + f.size.height) as i32), f.size.width as i32, f.size.height as i32)
            })
            .collect()
    }

    /// Moves `window` to its saved spot, kept fully on a screen.
    unsafe fn restore_position(window: *mut Object) {
        use cocoa::appkit::NSWindow;
        use cocoa::foundation::NSPoint;
        let Some((x, top)) = super::load_pos() else { return };
        let f = NSWindow::frame(window);
        let (w, h) = (f.size.width as i32, f.size.height as i32);
        // saved with y down (top edge negated): Cocoa's y points up from the main screen's bottom
        let (x, y) = super::fit_into(x, top, w, h, &screen_areas());
        NSWindow::setFrameOrigin_(window, NSPoint::new(x as f64, (-y - h) as f64));
    }

    /// Main thread only (AppKit).
    pub fn save_position() {
        use cocoa::appkit::NSWindow;
        let window = MAIN.load(Ordering::Relaxed);
        if window.is_null() {
            return;
        }
        unsafe {
            let f = NSWindow::frame(window);
            super::save_pos(f.origin.x as i32, -((f.origin.y + f.size.height) as i32));
        }
    }

    pub fn hide_window(handle: RawWindowHandle) {
        let RawWindowHandle::AppKit(_) = handle else { return };
        // baseview only puts the editor's view (and the standalone's own) into the window after the
        // editor's setup, so `[view window]` is still nil here: find the standalone's window among
        // the app's windows instead (the visible titled one; the splash is borderless)
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let windows: *mut Object = msg_send![app, windows];
            if windows.is_null() {
                return;
            }
            let count: usize = msg_send![windows, count];
            for i in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: i];
                let style: usize = msg_send![window, styleMask];
                let visible: objc::runtime::BOOL = msg_send![window, isVisible];
                if style & 1 /* NSWindowStyleMaskTitled */ != 0 && visible == objc::runtime::YES {
                    let _: () = msg_send![window, setAlphaValue: 0.0f64];
                    HIDDEN.store(window, Ordering::Relaxed);
                    MAIN.store(window, Ordering::Relaxed);
                    restore_position(window);
                    return;
                }
            }
        }
    }

    /// Main thread only (AppKit).
    pub fn reveal() {
        let window = HIDDEN.swap(std::ptr::null_mut(), Ordering::Relaxed);
        if window.is_null() {
            return;
        }
        unsafe {
            let _: () = msg_send![window, setAlphaValue: 1.0f64];
            let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: objc::runtime::YES];
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{hide_window, save_position};
#[cfg(target_os = "macos")]
pub use mac::reveal;

#[cfg(target_os = "linux")]
mod xwin {
    use raw_window_handle::RawWindowHandle;
    use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void, CStr};
    use std::sync::atomic::{AtomicU64, Ordering};
    use x11::xlib;

    static HIDDEN: AtomicU64 = AtomicU64::new(0);
    /// The standalone's top-level window (for `save_position`).
    static TOP: AtomicU64 = AtomicU64::new(0);

    /// A format-32 property of `window` (CARDINAL lists such as `_NET_FRAME_EXTENTS`).
    unsafe fn cardinals(dpy: *mut xlib::Display, window: c_ulong, name: &CStr) -> Vec<c_long> {
        let prop = xlib::XInternAtom(dpy, name.as_ptr(), xlib::True);
        if prop == 0 {
            return Vec::new();
        }
        let (mut ty, mut fmt, mut n, mut after, mut data): (c_ulong, c_int, c_ulong, c_ulong, *mut c_uchar) = (0, 0, 0, 0, std::ptr::null_mut());
        let ok = xlib::XGetWindowProperty(dpy, window, prop, 0, 64, xlib::False, xlib::XA_CARDINAL, &mut ty, &mut fmt, &mut n, &mut after, &mut data) == 0;
        let mut v = Vec::new();
        if ok && !data.is_null() && fmt == 32 {
            // format-32 data comes as an array of C longs
            v = std::slice::from_raw_parts(data as *const c_long, n as usize).to_vec();
        }
        if !data.is_null() {
            xlib::XFree(data as *mut _);
        }
        v
    }

    /// The window manager's frame around `window`: (left, right, top, bottom); 0s before it has one.
    unsafe fn frame_extents(dpy: *mut xlib::Display, window: c_ulong) -> (i32, i32, i32, i32) {
        match cardinals(dpy, window, c"_NET_FRAME_EXTENTS")[..] {
            [l, r, t, b, ..] => (l as i32, r as i32, t as i32, b as i32),
            _ => (0, 0, 0, 0),
        }
    }

    /// XRRMonitorInfo
    #[repr(C)]
    struct MonitorInfo {
        name: c_ulong,
        primary: c_int,
        automatic: c_int,
        noutput: c_int,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
        mwidth: c_int,
        mheight: c_int,
        outputs: *mut c_ulong,
    }
    extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(lib: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    /// The monitors' rectangles, primary first, from XRandR (loaded at run time, so it isn't a build
    /// dependency); the whole screen if that isn't available.
    unsafe fn monitors(dpy: *mut xlib::Display, root: c_ulong) -> Vec<(i32, i32, i32, i32)> {
        let screen = xlib::XDefaultScreen(dpy);
        let whole = vec![(0, 0, xlib::XDisplayWidth(dpy, screen), xlib::XDisplayHeight(dpy, screen))];
        let lib = dlopen(c"libXrandr.so.2".as_ptr(), 1 /* RTLD_LAZY */);
        if lib.is_null() {
            return whole;
        }
        let (get, free) = (dlsym(lib, c"XRRGetMonitors".as_ptr()), dlsym(lib, c"XRRFreeMonitors".as_ptr()));
        if get.is_null() || free.is_null() {
            return whole;
        }
        let get: unsafe extern "C" fn(*mut xlib::Display, c_ulong, c_int, *mut c_int) -> *mut MonitorInfo = std::mem::transmute(get);
        let free: unsafe extern "C" fn(*mut MonitorInfo) = std::mem::transmute(free);
        let mut n: c_int = 0;
        let list = get(dpy, root, xlib::True, &mut n);
        if list.is_null() {
            return whole;
        }
        let mut found: Vec<&MonitorInfo> = std::slice::from_raw_parts(list, n.max(0) as usize).iter().filter(|m| m.width > 0 && m.height > 0).collect();
        found.sort_by_key(|m| m.primary == 0);
        let v: Vec<_> = found.iter().map(|m| (m.x, m.y, m.width, m.height)).collect();
        free(list);
        if v.is_empty() { whole } else { v }
    }

    /// The monitors minus the panels (`_NET_WORKAREA`, where the window manager provides it).
    unsafe fn usable_areas(dpy: *mut xlib::Display, root: c_ulong) -> Vec<(i32, i32, i32, i32)> {
        let wa = match cardinals(dpy, root, c"_NET_WORKAREA")[..] {
            [x, y, w, h, ..] => Some((x as i32, y as i32, w as i32, h as i32)),
            _ => None,
        };
        monitors(dpy, root)
            .into_iter()
            .map(|m| {
                let Some(wa) = wa else { return m };
                let (l, t) = (m.0.max(wa.0), m.1.max(wa.1));
                let (r, b) = ((m.0 + m.2).min(wa.0 + wa.2), (m.1 + m.3).min(wa.1 + wa.3));
                if r > l && b > t { (l, t, r - l, b - t) } else { m }
            })
            .collect()
    }

    /// Moves `top` to its saved spot, kept fully on a monitor; with nothing saved yet (first
    /// start), centres it on the primary monitor.
    unsafe fn restore_position(dpy: *mut xlib::Display, top: c_ulong) {
        let mut attrs: xlib::XWindowAttributes = std::mem::zeroed();
        if xlib::XGetWindowAttributes(dpy, top, &mut attrs) == 0 {
            return;
        }
        let (l, r, t, b) = frame_extents(dpy, top);
        let (w, h) = (attrs.width + l + r, attrs.height + t + b);
        let areas = usable_areas(dpy, attrs.root);
        let (x, y) = match (super::load_pos(), areas.first()) {
            (Some(p), _) => p,
            (None, Some(a)) => (a.0 + (a.2 - w) / 2, a.1 + (a.3 - h) / 2),
            (None, None) => return,
        };
        let (x, y) = super::fit_into(x, y, w, h, &areas);
        // the position is the frame's top-left (NorthWest gravity); USPosition so window managers
        // honour it instead of placing the window themselves
        let mut hints: xlib::XSizeHints = std::mem::zeroed();
        let mut supplied: c_long = 0;
        xlib::XGetWMNormalHints(dpy, top, &mut hints, &mut supplied);
        hints.flags |= xlib::USPosition | xlib::PPosition;
        hints.x = x;
        hints.y = y;
        xlib::XSetWMNormalHints(dpy, top, &mut hints);
        xlib::XMoveWindow(dpy, top, x, y);
    }

    pub fn save_position() {
        let top = TOP.load(Ordering::Relaxed) as c_ulong;
        if top == 0 {
            return;
        }
        unsafe {
            let dpy = xlib::XOpenDisplay(std::ptr::null());
            if dpy.is_null() {
                return;
            }
            let mut attrs: xlib::XWindowAttributes = std::mem::zeroed();
            let (mut x, mut y, mut child): (c_int, c_int, c_ulong) = (0, 0, 0);
            if xlib::XGetWindowAttributes(dpy, top, &mut attrs) != 0
                && xlib::XTranslateCoordinates(dpy, top, attrs.root, 0, 0, &mut x, &mut y, &mut child) != 0
            {
                let (l, _, t, _) = frame_extents(dpy, top);
                super::save_pos(x - l, y - t);
            }
            xlib::XCloseDisplay(dpy);
        }
    }

    /// `_NET_WM_WINDOW_OPACITY` on `window` (0 = invisible), or the property removed (`None`).
    unsafe fn set_opacity(window: c_ulong, opacity: Option<u32>) {
        // our own connection: baseview's is busy on its event thread
        let dpy = xlib::XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            return;
        }
        let prop = xlib::XInternAtom(dpy, c"_NET_WM_WINDOW_OPACITY".as_ptr(), xlib::False);
        match opacity {
            Some(v) => {
                let data: [c_long; 1] = [v as c_long];
                xlib::XChangeProperty(dpy, window, prop, xlib::XA_CARDINAL, 32, xlib::PropModeReplace, data.as_ptr() as *const c_uchar, 1);
            }
            None => {
                xlib::XDeleteProperty(dpy, window, prop);
            }
        }
        xlib::XFlush(dpy);
        xlib::XCloseDisplay(dpy);
    }

    /// Not hidden on Linux (some compositors never showed the window again after the opacity hint
    /// was removed). Instead this makes the standalone's windows opaque: baseview always creates them
    /// with a 32-bit ARGB visual and never paints the top-level, so a compositor shows the unpainted
    /// window, and anything drawn with alpha, as see-through.
    pub fn hide_window(handle: RawWindowHandle) {
        let view: c_ulong = match handle {
            RawWindowHandle::Xlib(h) => h.window,
            RawWindowHandle::Xcb(h) => h.window as c_ulong,
            _ => return,
        };
        if view == 0 {
            return;
        }
        unsafe {
            let dpy = xlib::XOpenDisplay(std::ptr::null());
            if dpy.is_null() {
                return;
            }
            // our view is a child of the standalone's top-level window (which a window manager may
            // already have reparented into its frame, so don't walk further up than that)
            let (mut root, mut parent, mut children, mut n): (c_ulong, c_ulong, *mut c_ulong, c_uint) = (0, 0, std::ptr::null_mut(), 0);
            let ok = xlib::XQueryTree(dpy, view, &mut root, &mut parent, &mut children, &mut n) != 0;
            if !children.is_null() {
                xlib::XFree(children as *mut _);
            }
            let top = if ok && parent != 0 && parent != root { Some(parent) } else { None };
            // opaque black (alpha in the top byte of a 32-bit visual's pixel) behind our view. Only
            // the top-level: baseview ignores Expose, so a background on the view itself would have
            // the X server paint over our frame whenever the window manager maps or reframes it
            if let Some(top) = top {
                xlib::XSetWindowBackground(dpy, top, 0xFF00_0000);
                xlib::XClearWindow(dpy, top);
            }
            // and tell the compositor the whole top-level is opaque, whatever its alpha says
            if let Some(top) = top {
                let prop = xlib::XInternAtom(dpy, c"_NET_WM_OPAQUE_REGION".as_ptr(), xlib::False);
                let region: [c_long; 4] = [0, 0, 16384, 16384];
                xlib::XChangeProperty(dpy, top, prop, xlib::XA_CARDINAL, 32, xlib::PropModeReplace, region.as_ptr() as *const c_uchar, 4);
            }
            if let Some(top) = top {
                TOP.store(top as u64, Ordering::Relaxed);
                restore_position(dpy, top);
            }
            xlib::XFlush(dpy);
            xlib::XCloseDisplay(dpy);
        }
    }

    /// The opacity-0 hiding this module used to do (kept for reference; `reveal` undoes it).
    #[allow(dead_code)]
    fn hide_by_opacity(top: c_ulong) {
        unsafe { set_opacity(top, Some(0)) };
        HIDDEN.store(top as u64, Ordering::Relaxed);
    }

    pub fn reveal() {
        let window = HIDDEN.swap(0, Ordering::Relaxed);
        if window != 0 {
            unsafe { set_opacity(window as c_ulong, None) };
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use xwin::{hide_window, save_position};
#[cfg(target_os = "linux")]
pub use xwin::reveal;

#[cfg(not(windows))]
pub fn hide_next_main_window() {}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn reveal() {}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub(crate) fn hide_window(_handle: raw_window_handle::RawWindowHandle) {}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub(crate) fn save_position() {}
