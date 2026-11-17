//! Linux (X11): the card in a `_NET_WM_WINDOW_TYPE_SPLASH` window on its own Xlib connection and
//! thread (baseview's connection is busy on its own thread). Window managers centre splash windows,
//! keep them undecorated and don't give them focus. With a compositor running it uses a 32-bit ARGB
//! visual for the shadow, rounded corners and fades; without one, the plain opaque card.

use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint};
use std::time::{Duration, Instant};

use x11::xlib;

use super::{compose, FADE_IN, FADE_OUT, TIMEOUT};

pub fn show() {
    // from here on several threads use Xlib at once (this one, baseview's, the window icon and
    // reveal helpers), each with its own connection; Xlib wants this before any other call
    unsafe { xlib::XInitThreads() };
    std::thread::spawn(|| {
        // a splash that fails to draw is never worth breaking startup over
        let _ = std::panic::catch_unwind(run);
        // normally a no-op (the editor already revealed its window); after the timeout, or if the
        // splash couldn't be shown, it makes sure a slow start still shows the app
        crate::reveal::reveal();
    });
}

/// The desktop's scale from `Xft.dpi` (what GNOME/KDE set for HiDPI), else 1.
unsafe fn xft_scale(dpy: *mut xlib::Display) -> f32 {
    let res = xlib::XResourceManagerString(dpy);
    if res.is_null() {
        return 1.0;
    }
    let res = std::ffi::CStr::from_ptr(res).to_string_lossy();
    res.lines()
        .find_map(|l| l.strip_prefix("Xft.dpi:").and_then(|v| v.trim().parse::<f32>().ok()))
        .map(|dpi| (dpi / 96.0).max(1.0))
        .unwrap_or(1.0)
}

unsafe fn atom(dpy: *mut xlib::Display, name: &str) -> xlib::Atom {
    let name = std::ffi::CString::new(name).unwrap();
    xlib::XInternAtom(dpy, name.as_ptr(), xlib::False)
}

unsafe fn set_atoms(dpy: *mut xlib::Display, win: xlib::Window, prop: &str, values: &[xlib::Atom]) {
    // format-32 property data is passed to Xlib as C longs
    let data: Vec<c_long> = values.iter().map(|&a| a as c_long).collect();
    xlib::XChangeProperty(dpy, win, atom(dpy, prop), xlib::XA_ATOM, 32, xlib::PropModeReplace, data.as_ptr() as *const c_uchar, data.len() as c_int);
}

fn run() {
    unsafe {
        let dpy = xlib::XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            return;
        }
        let screen = xlib::XDefaultScreen(dpy);
        let root = xlib::XRootWindow(dpy, screen);

        // translucent only with a compositing manager and a 32-bit visual
        let composited = xlib::XGetSelectionOwner(dpy, atom(dpy, &format!("_NET_WM_CM_S{screen}"))) != 0;
        let mut vinfo: xlib::XVisualInfo = std::mem::zeroed();
        let argb = composited && xlib::XMatchVisualInfo(dpy, screen, 32, xlib::TrueColor, &mut vinfo) != 0;
        let (visual, depth) = if argb { (vinfo.visual, 32) } else { (xlib::XDefaultVisual(dpy, screen), xlib::XDefaultDepth(dpy, screen)) };
        if depth != 24 && depth != 32 {
            xlib::XCloseDisplay(dpy);
            return;
        }

        let Some(card) = compose(xft_scale(dpy), argb) else {
            xlib::XCloseDisplay(dpy);
            return;
        };
        let (w, h) = (card.w as c_uint, card.h as c_uint);

        let colormap = xlib::XCreateColormap(dpy, root, visual, xlib::AllocNone);
        let mut attrs: xlib::XSetWindowAttributes = std::mem::zeroed();
        attrs.colormap = colormap;
        attrs.border_pixel = 0;
        attrs.background_pixel = 0;
        attrs.event_mask = xlib::ExposureMask;
        // centred on the screen; window managers usually place splash windows themselves
        let (sw, sh) = (xlib::XDisplayWidth(dpy, screen), xlib::XDisplayHeight(dpy, screen));
        let (x, y) = ((sw - w as c_int) / 2, (sh - h as c_int) / 2);
        let win = xlib::XCreateWindow(
            dpy,
            root,
            x,
            y,
            w,
            h,
            0,
            depth,
            xlib::InputOutput as c_uint,
            visual,
            xlib::CWColormap | xlib::CWBorderPixel | xlib::CWBackPixel | xlib::CWEventMask,
            &mut attrs,
        );

        let title = c"Jwala";
        xlib::XStoreName(dpy, win, title.as_ptr() as *mut c_char);
        let class = b"jwala\0Jwala\0";
        xlib::XChangeProperty(dpy, win, xlib::XA_WM_CLASS, xlib::XA_STRING, 8, xlib::PropModeReplace, class.as_ptr(), class.len() as c_int);
        set_atoms(dpy, win, "_NET_WM_WINDOW_TYPE", &[atom(dpy, "_NET_WM_WINDOW_TYPE_SPLASH")]);
        set_atoms(dpy, win, "_NET_WM_STATE", &[atom(dpy, "_NET_WM_STATE_ABOVE"), atom(dpy, "_NET_WM_STATE_SKIP_TASKBAR")]);
        // no decorations, for window managers that ignore the splash type (flags = decorations, none)
        let motif: [c_long; 5] = [2, 0, 0, 0, 0];
        let motif_atom = atom(dpy, "_MOTIF_WM_HINTS");
        xlib::XChangeProperty(dpy, win, motif_atom, motif_atom, 32, xlib::PropModeReplace, motif.as_ptr() as *const c_uchar, 5);
        let hints = xlib::XAllocSizeHints();
        if !hints.is_null() {
            (*hints).flags = xlib::PPosition | xlib::PMinSize | xlib::PMaxSize;
            (*hints).x = x;
            (*hints).y = y;
            (*hints).min_width = w as c_int;
            (*hints).max_width = w as c_int;
            (*hints).min_height = h as c_int;
            (*hints).max_height = h as c_int;
            xlib::XSetWMNormalHints(dpy, win, hints);
            xlib::XFree(hints as *mut _);
        }

        // a ZPixmap XImage over our own buffer: premultiplied BGRA is X11's little-endian ARGB32
        // (for depth 24 the alpha byte is ignored)
        let mut buf = Vec::new();
        card.bgra(if argb { 0.0 } else { 1.0 }, &mut buf);
        let img = xlib::XCreateImage(dpy, visual, depth as c_uint, xlib::ZPixmap, 0, buf.as_mut_ptr() as *mut c_char, w, h, 32, 0);
        let gc = xlib::XCreateGC(dpy, win, 0, std::ptr::null_mut());
        if img.is_null() || gc.is_null() {
            xlib::XDestroyWindow(dpy, win);
            xlib::XFreeColormap(dpy, colormap);
            xlib::XCloseDisplay(dpy);
            return;
        }
        xlib::XMapRaised(dpy, win);
        xlib::XFlush(dpy);

        let start = Instant::now();
        let mut closing: Option<Instant> = None;
        let mut shown_alpha = -1.0f32;
        loop {
            let mut exposed = false;
            while xlib::XPending(dpy) > 0 {
                let mut ev: xlib::XEvent = std::mem::zeroed();
                xlib::XNextEvent(dpy, &mut ev);
                exposed |= ev.get_type() == xlib::Expose;
            }
            if closing.is_none() && (crate::UI_READY.load(std::sync::atomic::Ordering::Relaxed) || start.elapsed() > TIMEOUT) {
                closing = Some(Instant::now());
            }
            let fade_in = (start.elapsed().as_secs_f32() / FADE_IN).min(1.0);
            let alpha = match closing {
                // without a compositor there's no fading: it just goes
                Some(_) if !argb => break,
                Some(t) => {
                    let out = t.elapsed().as_secs_f32() / FADE_OUT;
                    if out >= 1.0 {
                        break;
                    }
                    fade_in * (1.0 - out)
                }
                None if argb => fade_in,
                None => 1.0,
            };
            if alpha != shown_alpha || exposed {
                if alpha != shown_alpha {
                    card.bgra(alpha, &mut buf);
                    (*img).data = buf.as_mut_ptr() as *mut c_char;
                    shown_alpha = alpha;
                }
                xlib::XPutImage(dpy, win, gc, img, 0, 0, 0, 0, w, h);
                xlib::XFlush(dpy);
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        // the buffer is ours: detach it before XDestroyImage frees the image
        (*img).data = std::ptr::null_mut();
        xlib::XDestroyImage(img);
        xlib::XFreeGC(dpy, gc);
        xlib::XDestroyWindow(dpy, win);
        xlib::XFreeColormap(dpy, colormap);
        xlib::XCloseDisplay(dpy);
    }
}
