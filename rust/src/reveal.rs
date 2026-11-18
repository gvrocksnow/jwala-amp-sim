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
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const c_void, size: u32) -> i32;
    }

    const WH_CBT: i32 = 5;
    const WH_CALLWNDPROCRET: i32 = 12;
    const HCBT_CREATEWND: i32 = 3;
    const WM_CREATE: u32 = 1;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const DWMWA_CLOAK: u32 = 13;

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
            cloak(hwnd, false);
            unsafe { SetForegroundWindow(hwnd) };
        }
    }
}

#[cfg(windows)]
pub use win::{hide_next_main_window, reveal};

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
pub(crate) use mac::hide_window;
#[cfg(target_os = "macos")]
pub use mac::reveal;

#[cfg(target_os = "linux")]
mod xwin {
    use raw_window_handle::RawWindowHandle;
    use std::ffi::{c_long, c_uchar, c_uint, c_ulong};
    use std::sync::atomic::{AtomicU64, Ordering};
    use x11::xlib;

    static HIDDEN: AtomicU64 = AtomicU64::new(0);

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
            // opaque black (alpha in the top byte of a 32-bit visual's pixel) behind everything
            for w in [Some(view), top].into_iter().flatten() {
                xlib::XSetWindowBackground(dpy, w, 0xFF00_0000);
                xlib::XClearWindow(dpy, w);
            }
            // and tell the compositor the whole top-level is opaque, whatever its alpha says
            if let Some(top) = top {
                let prop = xlib::XInternAtom(dpy, c"_NET_WM_OPAQUE_REGION".as_ptr(), xlib::False);
                let region: [c_long; 4] = [0, 0, 16384, 16384];
                xlib::XChangeProperty(dpy, top, prop, xlib::XA_CARDINAL, 32, xlib::PropModeReplace, region.as_ptr() as *const c_uchar, 4);
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
pub(crate) use xwin::hide_window;
#[cfg(target_os = "linux")]
pub use xwin::reveal;

#[cfg(not(windows))]
pub fn hide_next_main_window() {}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn reveal() {}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub(crate) fn hide_window(_handle: raw_window_handle::RawWindowHandle) {}
