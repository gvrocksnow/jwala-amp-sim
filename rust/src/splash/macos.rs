//! macOS: the card in a borderless, transparent NSWindow above normal windows. AppKit only works on
//! the main thread, which then spends the load inside the editor's setup without running the event
//! loop, so the splash is drawn and flushed to the window server up front and needs nothing from
//! the run loop to stay up; the editor's frame callback then fades and closes it (`frame`).

use std::cell::RefCell;
use std::time::Instant;

use cocoa::base::{id, nil, NO, YES};
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
use objc::runtime::Class;
use objc::{class, msg_send, sel, sel_impl};

use super::{compose, FADE_OUT, TIMEOUT};

struct Splash {
    window: id,
    shown: Instant,
    closing: Option<Instant>,
}

thread_local! {
    static SPLASH: RefCell<Option<Splash>> = const { RefCell::new(None) };
}

pub fn show() {
    unsafe {
        // what baseview does first too; needed before any window exists
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, setActivationPolicy: 0isize /* NSApplicationActivationPolicyRegular */];

        let screen: id = msg_send![class!(NSScreen), mainScreen];
        if screen == nil {
            return;
        }
        let scale: f64 = msg_send![screen, backingScaleFactor];
        let Some(card) = compose(scale as f32, true) else { return };
        let (pw, ph) = (card.w as f64 / scale, card.h as f64 / scale);

        // the card as a bitmap at the screen's backing scale, shown at its size in points
        let space = NSString::alloc(nil).init_str("NSDeviceRGBColorSpace");
        let rep: id = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: id = msg_send![rep,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>()
            pixelsWide: card.w as isize
            pixelsHigh: card.h as isize
            bitsPerSample: 8isize
            samplesPerPixel: 4isize
            hasAlpha: YES
            isPlanar: NO
            colorSpaceName: space
            bytesPerRow: (card.w * 4) as isize
            bitsPerPixel: 32isize];
        let _: () = msg_send![space, release];
        if rep == nil {
            return;
        }
        let data: *mut u8 = msg_send![rep, bitmapData];
        if data.is_null() {
            let _: () = msg_send![rep, release];
            return;
        }
        // NSBitmapImageRep's default layout is premultiplied RGBA, alpha last
        let rgba = card.rgba();
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), data, rgba.len());
        let image: id = msg_send![class!(NSImage), alloc];
        let image: id = msg_send![image, initWithSize: NSSize::new(pw, ph)];
        let _: () = msg_send![image, addRepresentation: rep];
        let _: () = msg_send![rep, release];

        // centred on the main screen's usable area
        let area: NSRect = msg_send![screen, visibleFrame];
        let rect = NSRect::new(
            NSPoint::new((area.origin.x + (area.size.width - pw) / 2.0).round(), (area.origin.y + (area.size.height - ph) / 2.0).round()),
            NSSize::new(pw, ph),
        );
        let window: id = msg_send![class!(NSWindow), alloc];
        let window: id = msg_send![window,
            initWithContentRect: rect
            styleMask: 0usize /* NSWindowStyleMaskBorderless */
            backing: 2usize /* NSBackingStoreBuffered */
            defer: NO];
        if window == nil {
            let _: () = msg_send![image, release];
            return;
        }
        let clear: id = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setOpaque: NO];
        let _: () = msg_send![window, setBackgroundColor: clear];
        // the card draws its own shadow
        let _: () = msg_send![window, setHasShadow: NO];
        let _: () = msg_send![window, setLevel: 3isize /* NSFloatingWindowLevel */];
        let _: () = msg_send![window, setIgnoresMouseEvents: YES];
        let _: () = msg_send![window, setReleasedWhenClosed: NO];

        let view: id = msg_send![class!(NSImageView), alloc];
        let view: id = msg_send![view, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pw, ph))];
        let _: () = msg_send![view, setImageScaling: 1usize /* NSImageScaleAxesIndependently */];
        let _: () = msg_send![view, setImage: image];
        let _: () = msg_send![window, setContentView: view];
        let _: () = msg_send![view, release];
        let _: () = msg_send![image, release];

        let _: () = msg_send![window, orderFrontRegardless];
        let _: () = msg_send![window, display];
        // push it to the window server now: the run loop that would normally do this doesn't run
        // until the editor has finished loading
        if let Some(ca) = Class::get("CATransaction") {
            let _: () = msg_send![ca, flush];
        }

        SPLASH.with(|s| *s.borrow_mut() = Some(Splash { window, shown: Instant::now(), closing: None }));
    }
}

/// Fades the splash out once the editor has drawn (or after the timeout), then closes it.
pub fn frame() {
    SPLASH.with(|s| {
        let mut s = s.borrow_mut();
        let Some(sp) = s.as_mut() else { return };
        if sp.closing.is_none() && (crate::UI_READY.load(std::sync::atomic::Ordering::Relaxed) || sp.shown.elapsed() > TIMEOUT) {
            sp.closing = Some(Instant::now());
            crate::reveal::reveal();
        }
        let Some(t) = sp.closing else { return };
        let out = t.elapsed().as_secs_f32() / FADE_OUT;
        unsafe {
            if out >= 1.0 {
                let _: () = msg_send![sp.window, orderOut: nil];
                let _: () = msg_send![sp.window, close];
                let _: () = msg_send![sp.window, release];
                *s = None;
            } else {
                let _: () = msg_send![sp.window, setAlphaValue: (1.0 - out) as f64];
            }
        }
    });
}
