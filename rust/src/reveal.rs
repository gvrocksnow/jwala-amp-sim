//! Keeps the standalone's main window invisible until the editor has drawn its first frame, so the
//! user sees the splash and then the finished UI, never a blank window.
//!
//! baseview creates the top-level window already visible (`WS_VISIBLE` is hard-coded), so a
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

#[cfg(not(windows))]
pub fn hide_next_main_window() {}
#[cfg(not(windows))]
pub fn reveal() {}
