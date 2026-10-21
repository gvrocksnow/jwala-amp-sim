//! Everything on screen, ported from the C# WinForms pages (HeaderBar, PedalPage, AmpPanel,
//! CabPage, TunerPanel). Drawn every frame; each frame also records the click regions.

use super::painter::{rgb, rgba, with_alpha, Color, Painter, Rect, ShadowTerm, TexId};
use super::text::{self, Font};
use super::{App, Field, Hit};
use crate::params::Knob;
use crate::presets;
use crate::spec::{self, EffectSpec};
use std::sync::atomic::Ordering;

const GOLD: Color = [214.0 / 255.0, 178.0 / 255.0, 96.0 / 255.0, 1.0];
const TEXT: Color = [235.0 / 255.0, 225.0 / 255.0, 205.0 / 255.0, 1.0];
const DIM: Color = [150.0 / 255.0, 140.0 / 255.0, 125.0 / 255.0, 1.0];
const ROW1: f32 = 66.0;
const ROW2: f32 = 50.0;
const HEADER: f32 = ROW1 + ROW2;

pub fn draw(app: &mut App, p: &mut Painter, dt: f64) {
    let (w, h) = app.size();
    // anything beyond the layout (spare room while the window is being resized) in the frame colour
    p.fill(Rect::new(0.0, 0.0, 1e5, 1e5), rgb(16, 14, 12));
    let body = Rect::new(0.0, HEADER, w, h - HEADER);
    if app.tuner_open {
        tuner(app, p, body, dt);
    } else {
        match app.page {
            0 => pedal_page(app, p, body, false),
            1 => amp_page(app, p, body),
            2 => cab_page(app, p, body),
            3 => eq_page(app, p, body),
            _ => pedal_page(app, p, body, true),
        }
    }
    header(app, p, w);
    if let Some((msg, _)) = &app.message {
        let msg = msg.clone();
        let tw = text::measure(Font::Semibold, 9.5, &msg) + 32.0;
        let r = Rect::new((w - tw) / 2.0, h - 54.0, tw, 32.0);
        p.rrect(r, 8.0, rgba(24, 21, 19, 235));
        p.rrect_stroke(r, 8.0, 1.0, with_alpha(GOLD, 140));
        p.text_in(Font::Semibold, 9.5, &msg, r, 0.5, 0.5, TEXT);
    }
    preset_menu(app, p);
    dialog(app, p, w, h);
    audio_settings(app, p, w, h);
    // resize grip
    let g = Rect::new(w - 18.0, h - 18.0, 18.0, 18.0);
    for k in 0..3 {
        let o = 4.0 + k as f32 * 4.0;
        p.line(g.right() - 3.0, g.bottom() - o, g.right() - o, g.bottom() - 3.0, 1.4, rgba(255, 255, 255, 70));
    }
    app.hits.push((g, Hit::Resize));
}

// ===================================================================== widgets

fn hot(app: &App, h: Hit) -> bool {
    app.hot == Some(h) && app.pressed.is_none()
}

fn pressed(app: &App, h: Hit) -> bool {
    app.pressed == Some(h)
}

/// GDI+ `Gfx.Led`.
fn led(p: &mut Painter, r: Rect, on: bool, c: Color) {
    if on {
        p.glow(r.inflate(r.w * 0.9, r.h * 0.9), with_alpha(c, 190), with_alpha(c, 0), 0.0);
    }
    p.ellipse(r, if on { c } else { [c[0] / 4.0, c[1] / 4.0, c[2] / 4.0, 1.0] });
    p.ellipse(Rect::new(r.x + r.w * 0.25, r.y + r.h * 0.18, r.w * 0.35, r.h * 0.3), rgba(255, 255, 255, if on { 200 } else { 70 }));
    p.ellipse_stroke(r, 1.0, rgba(0, 0, 0, 140));
}

fn button(app: &mut App, p: &mut Painter, r: Rect, label: &str, hit: Hit, active: bool, pt: f32) {
    let (h, pr) = (hot(app, hit), pressed(app, hit));
    let bg = if pr { rgb(70, 62, 50) } else if active { rgb(78, 64, 36) } else if h { rgb(56, 50, 44) } else { rgb(40, 36, 32) };
    p.rrect(r, 5.0, bg);
    p.rrect_stroke(r, 5.0, 1.0, if active { GOLD } else { rgb(80, 72, 60) });
    p.text_in(Font::Bold, pt, label, r, 0.5, 0.5, if active { rgb(255, 230, 170) } else { TEXT });
    app.hits.push((r, hit));
}

/// Latching switch with an LED (channel buttons, cab on…).
fn switch(app: &mut App, p: &mut Painter, r: Rect, label: &str, hit: Hit, on: bool, led_c: Color) {
    let h = hot(app, hit);
    let top = if on { rgb(66, 58, 48) } else if h { rgb(46, 41, 36) } else { rgb(34, 30, 26) };
    p.grad(r, 5.0, top, rgb(12, 10, 8));
    p.rrect_stroke(r, 5.0, 1.2, if on { with_alpha(led_c, 120) } else { rgba(0, 0, 0, 140) });
    led(p, Rect::new(r.x + 8.0, r.cy() - 3.5, 7.0, 7.0), on, led_c);
    p.text_in(Font::ArialBold, 7.5, label, Rect::new(r.x + 16.0, r.y, r.w - 18.0, r.h), 0.5, 0.5, if on { rgb(245, 235, 210) } else { DIM });
    app.hits.push((r, hit));
}

struct KnobLook {
    ticks: Option<Color>,
    /// The page bakes the shadow (pedal pages), so draw none here.
    baked_shadow: bool,
}

fn knob(app: &mut App, p: &mut Painter, r: Rect, k: Knob, img: TexId, look: KnobLook) {
    let v = app.params.knob(k).value();
    let (cx, cy) = (r.cx(), r.cy());
    if let Some(tc) = look.ticks {
        let (r0, r1) = (r.w / 2.0 + 3.0, r.w / 2.0 + (r.w * 0.12).max(6.0));
        for t in 0..=10 {
            let a = ((-150.0 + 30.0 * t as f32 - 90.0) as f32).to_radians();
            let len = if t % 5 == 0 { r1 + 3.0 } else { r1 };
            p.line(cx + a.cos() * r0, cy + a.sin() * r0, cx + a.cos() * len, cy + a.sin() * len, 1.6, tc);
        }
    }
    if !look.baked_shadow {
        p.ellipse(Rect::new(r.x + r.w * 0.04, r.y + r.h * 0.09, r.w, r.h), rgba(0, 0, 0, 110));
    }
    p.image(img, r);
    let ang = (-150.0 + 300.0 * v - 90.0f32).to_radians();
    let (p0, p1) = (r.w * 0.1, r.w * 0.44);
    let (x0, y0, x1, y1) = (cx + ang.cos() * p0, cy + ang.sin() * p0, cx + ang.cos() * p1, cy + ang.sin() * p1);
    let w = (r.w * 0.035).max(1.6);
    p.line(x0, y0, x1, y1, w + 2.5, rgba(0, 0, 0, 150));
    p.line(x0, y0, x1, y1, w, rgb(250, 248, 240));
    app.hits.push((r.inflate(6.0, 6.0), Hit::Knob(k)));
}

/// Value bubble under the hovered or dragged knob (drawn last so it sits on top).
fn knob_tooltips(app: &App, p: &mut Painter) {
    let target = match (app.pressed, app.hot) {
        (Some(Hit::Knob(k)), _) => Some(k),
        (None, Some(Hit::Knob(k))) => Some(k),
        _ => None,
    };
    let Some(k) = target else { return };
    let Some((r, _)) = app.hits.iter().find(|(_, h)| *h == Hit::Knob(k)) else { return };
    let r = r.inflate(-6.0, -6.0);
    let s = (app.params.knob_spec(k).fmt)(app.params.knob(k).value());
    let tw = text::measure(Font::Bold, 8.5, &s);
    let b = Rect::new(r.cx() - tw / 2.0 - 5.0, r.bottom() + 4.0, tw + 10.0, 17.0);
    p.rrect(b, 5.0, rgba(18, 15, 12, if app.pressed.is_some() { 235 } else { 190 }));
    p.text_in(Font::Bold, 8.5, &s, b, 0.5, 0.5, GOLD);
}

fn meter(p: &mut Painter, r: Rect, peak: f32) {
    let db = 20.0 * (peak + 1e-6).log10();
    p.fill(r, rgb(14, 12, 10));
    const SEGS: usize = 16;
    let sl = (r.h - 2.0) / SEGS as f32;
    for i in 0..SEGS {
        let seg_db = -60.0 + i as f32 * 3.75;
        let c = if i >= SEGS - 2 { rgb(255, 50, 40) } else if i >= SEGS - 5 { rgb(255, 190, 40) } else { rgb(70, 220, 90) };
        let c = if db >= seg_db { c } else { with_alpha(c, 35) };
        p.fill(Rect::new(r.x + 1.0, r.bottom() - 1.0 - (i + 1) as f32 * sl + 1.0, r.w - 2.0, sl - 1.5), c);
    }
}

fn vignette(p: &mut Painter, r: Rect, alpha: u8) {
    p.push_clip(r);
    p.glow(r.inflate(r.w * 0.25, r.h * 0.4), rgba(0, 0, 0, 0), rgba(0, 0, 0, alpha), 0.0);
    p.pop_clip();
}

/// GDI+ `Gfx.Shadow`: three stacked translucent rounded rects.
fn box_shadow(p: &mut Painter, r: Rect, rad: f32, alpha: u8, dx: f32, dy: f32) {
    for i in (1..=3).rev() {
        let i = i as f32;
        p.rrect(Rect::new(r.x + dx - i * 2.0, r.y + dy - i * 2.0, r.w + i * 4.0, r.h + i * 4.0), rad + i * 2.0, rgba(0, 0, 0, (alpha as f32 / (i + 1.0)) as u8));
    }
}

// ===================================================================== header

fn header(app: &mut App, p: &mut Painter, w: f32) {
    p.grad(Rect::new(0.0, 0.0, w, ROW1), 0.0, rgb(34, 30, 27), rgb(18, 16, 14));
    p.fill(Rect::new(0.0, ROW1, w, ROW2), rgb(12, 11, 10));
    p.fill(Rect::new(0.0, ROW1, w, 1.0), with_alpha(GOLD, 70));
    p.fill(Rect::new(0.0, HEADER - 1.0, w, 1.0), with_alpha(GOLD, 70));
    p.logo(Rect::new(14.0, 6.0, 260.0, 52.0), 20.0, 0.0);
    // version, baseline-aligned just after the logo script
    let lm = text::line_metrics(Font::Script, 20.0);
    let base = 6.0 + (52.0 - lm.height()) / 2.0 + lm.ascent;
    let vx = 18.0 + text::measure(Font::Script, 20.0, "Jwala") + 8.0;
    p.text(Font::ArialBold, 14.0, concat!("v", env!("CARGO_PKG_VERSION")), vx, base - text::line_metrics(Font::ArialBold, 14.0).ascent, DIM);

    // right side: (standalone) settings and power, then meters and global knobs
    let mut x = w - 16.0;
    if app.audio.is_some() {
        x -= 34.0;
        let gear = Rect::new(x, 16.0, 34.0, 34.0);
        p.ellipse(gear, if hot(app, Hit::Gear) { rgb(50, 46, 42) } else { rgb(32, 29, 26) });
        p.text_in(Font::Regular, 14.0, "⚙", gear, 0.5, 0.5, TEXT);
        app.hits.push((gear, Hit::Gear));
        x -= 44.0;
        let pw = Rect::new(x, 16.0, 34.0, 34.0);
        let on = app.audio_running();
        let c = if on { rgb(90, 230, 110) } else { rgb(230, 70, 50) };
        if on {
            p.glow(pw.inflate(8.0, 8.0), with_alpha(c, 90), with_alpha(c, 0), 0.0);
        }
        p.ellipse(pw, if hot(app, Hit::Power) { rgb(50, 46, 42) } else { rgb(32, 29, 26) });
        let ir = pw.inflate(-9.0, -9.0);
        p.arc(ir.cx(), ir.cy(), ir.w / 2.0, -60.0, 300.0, 2.4, c);
        p.line(pw.cx(), ir.y - 2.0, pw.cx(), pw.cy(), 2.4, c);
        app.hits.push((pw, Hit::Power));
        x -= 14.0;
    } else {
        x -= 16.0;
    }
    let meter_r = Rect::new(x, 12.0, 8.0, 42.0);
    let meter_l = Rect::new(x - 11.0, 12.0, 8.0, 42.0);
    x -= 47.0;
    let meter_in = Rect::new(x, 12.0, 8.0, 42.0);
    x -= 16.0;
    let pk = app.tex("pedal_knob.png");
    for i in (0..3).rev() {
        x -= 58.0;
        let r = Rect::new(x + 12.0, 7.0, 34.0, 34.0);
        knob(app, p, r, Knob::Global(i), pk, KnobLook { ticks: None, baked_shadow: false });
        p.text_in(Font::ArialBold, 6.5, spec::GLOBALS[i].name, Rect::new(r.x - 14.0, r.bottom() + 2.0, r.w + 28.0, 12.0), 0.5, 0.5, DIM);
    }
    let sh = &app.shared;
    meter(p, meter_in, sh.meter_in.load(Ordering::Relaxed));
    meter(p, meter_l, sh.meter_l.load(Ordering::Relaxed));
    meter(p, meter_r, sh.meter_r.load(Ordering::Relaxed));
    p.text_in(Font::ArialBold, 6.5, "IN", Rect::new(meter_in.x - 8.0, meter_in.bottom() + 1.0, 24.0, 10.0), 0.5, 0.5, DIM);
    p.text_in(Font::ArialBold, 6.5, "OUT", Rect::new(meter_l.x - 12.0, meter_l.bottom() + 1.0, 42.0, 10.0), 0.5, 0.5, DIM);

    // preset group centred between the logo and the global knobs
    let cx = (270.0 + x) / 2.0 - 40.0;
    button(app, p, Rect::new(cx - 190.0, 17.0, 32.0, 32.0), "◀", Hit::PresetPrev, false, 8.5);
    let pr = Rect::new(cx - 152.0, 17.0, 304.0, 32.0);
    p.rrect(pr, 6.0, if hot(app, Hit::PresetMenu) { rgb(44, 40, 36) } else { rgb(30, 27, 24) });
    p.rrect_stroke(pr, 6.0, 1.0, with_alpha(GOLD, 110));
    let name = format!("{}{}", app.preset_name, if app.dirty { " *" } else { "" });
    p.text_fit(Font::Bold, 10.5, &name, Rect::new(pr.x + 10.0, pr.y, pr.w - 34.0, pr.h), 0.5, TEXT);
    p.text_in(Font::Bold, 10.5, "▾", Rect::new(pr.right() - 26.0, pr.y, 20.0, pr.h), 0.5, 0.5, GOLD);
    app.hits.push((pr, Hit::PresetMenu));
    button(app, p, Rect::new(cx + 158.0, 17.0, 32.0, 32.0), "▶", Hit::PresetNext, false, 8.5);
    button(app, p, Rect::new(cx + 198.0, 17.0, 62.0, 32.0), "SAVE", Hit::Save, false, 8.5);
    let tuner_open = app.tuner_open;
    button(app, p, Rect::new(cx + 268.0, 17.0, 76.0, 32.0), "♩ TUNER", Hit::Tuner, tuner_open, 8.5);

    // row 2: signal chain navigation
    p.text(Font::Bold, 7.5, "SIGNAL CHAIN", 18.0, ROW1 + 18.0, DIM);
    const PAGES: [&str; 5] = ["PEDALS", "AMP", "CAB", "EQ", "EFFECTS"];
    let (tw, gap) = (140.0, 30.0);
    let total = tw * PAGES.len() as f32 + gap * (PAGES.len() - 1) as f32;
    let tx = (w - total) / 2.0;
    for i in 0..PAGES.len() {
        let b = Rect::new(tx + i as f32 * (tw + gap), ROW1 + 6.0, tw, ROW2 - 12.0);
        let active = app.page == i && !app.tuner_open;
        let h = hot(app, Hit::Tab(i));
        p.rrect(b, 6.0, if active { rgb(58, 48, 30) } else if h { rgb(38, 34, 30) } else { rgb(26, 23, 20) });
        p.rrect_stroke(b, 6.0, if active { 1.5 } else { 1.0 }, if active { GOLD } else { rgb(60, 54, 46) });
        p.text_in(Font::Bold, 8.5, PAGES[i], Rect::new(b.x, b.y + 2.0, b.w, b.h / 2.0), 0.5, 0.5, if active { rgb(255, 230, 170) } else { TEXT });
        let sub = Rect::new(b.x, b.y + b.h / 2.0, b.w, b.h / 2.0 - 2.0);
        if i == 3 && app.params.eq_on.value() {
            // the slider positions in miniature
            let bw = 7.0;
            let start = b.x + (b.w - bw * spec::EQ_BANDS as f32) / 2.0;
            let (mid, half) = (sub.cy(), sub.h / 2.0 - 3.0);
            p.fill(Rect::new(start, mid - 0.5, bw * spec::EQ_BANDS as f32, 1.0), rgba(255, 255, 255, 50));
            for (k, f) in app.params.eq.iter().take(spec::EQ_BANDS).enumerate() {
                let d = (f.value() - 0.5) * 2.0 * half;
                let bar = Rect::new(start + k as f32 * bw + 1.5, mid - d.max(0.0), bw - 3.0, d.abs().max(1.5));
                p.rrect(bar, 1.0, rgba(255, 120, 40, 220));
            }
        } else if i == 3 {
            p.text_in(Font::Bold, 7.0, "BYPASSED", sub, 0.5, 0.5, DIM);
        } else if i == 0 || i == 4 {
            let fx = if i == 0 { &app.params.pre } else { &app.params.post };
            let dw = ((b.w - 12.0) / fx.len() as f32).min(30.0);
            let start = b.x + (b.w - dw * fx.len() as f32) / 2.0;
            for (k, f) in fx.iter().enumerate() {
                let d = Rect::new(start + k as f32 * dw + 3.0, sub.y + 3.0, dw - 6.0, sub.h - 6.0);
                p.rrect(d, 3.0, if f.on.value() { rgba(255, 120, 40, 200) } else { rgba(255, 255, 255, 50) });
            }
        } else {
            let s = if i == 1 {
                let ch = spec::CHANNELS[app.params.channel.value().clamp(0, 3) as usize];
                if app.params.bright.value() { format!("{ch} · BRIGHT") } else { ch.to_string() }
            } else if !app.params.cab_on.value() {
                "BYPASSED".to_string()
            } else if app.params.channel.value() == spec::ACOUSTIC {
                "MIC ONLY".to_string()
            } else {
                spec::CAB_MODELS[app.params.cab_model.value().clamp(0, 3) as usize].to_string()
            };
            p.text_in(Font::Bold, 7.0, &s, sub, 0.5, 0.5, DIM);
        }
        if i + 1 < PAGES.len() {
            p.text_in(Font::Bold, 12.0, "→", Rect::new(b.right(), b.y, gap, b.h), 0.5, 0.5, with_alpha(GOLD, 120));
        }
        app.hits.push((b, Hit::Tab(i)));
    }
    let sr = app.shared.sample_rate.load(Ordering::Relaxed);
    let status = if let Some(a) = &app.audio {
        if a.host.running() { format!("{}  ·  CPU {:.0}%", a.host.status(), app.shared.cpu.load(Ordering::Relaxed) * 100.0) } else { a.error.clone().unwrap_or_else(|| a.host.status()) }
    } else if sr > 0.0 {
        format!("{:.1} kHz · {} smp · CPU {:.0}%", sr / 1000.0, app.shared.block_size.load(Ordering::Relaxed), app.shared.cpu.load(Ordering::Relaxed) * 100.0)
    } else {
        "audio stopped".to_string()
    };
    let st = Rect::new(tx + total + 20.0, ROW1, w - (tx + total + 20.0) - 24.0, ROW2);
    let status = if app.software_render() { format!("{status}  ·  CPU rendering (no GPU)") } else { status };
    let bad = app.audio.as_ref().is_some_and(|a| !a.host.running());
    p.text_fit(Font::Regular, 7.5, &status, st, 1.0, if bad { rgb(230, 120, 90) } else { DIM });
    knob_tooltips(app, p);
}

// ===================================================================== pedal pages

const SLATE: Color = [42.0 / 255.0, 45.0 / 255.0, 50.0 / 255.0, 1.0];
const INK: Color = [34.0 / 255.0, 36.0 / 255.0, 42.0 / 255.0, 225.0 / 255.0];

/// Accent colours matching the pastel enclosures.
fn accent(id: &str) -> Color {
    match id {
        "comp" => rgb(227, 180, 179),
        "od" => rgb(173, 211, 196),
        "dist" => rgb(152, 191, 213),
        "fuzz" => rgb(222, 204, 153),
        "doubler" => rgb(220, 180, 188),
        "chorus" => rgb(157, 209, 205),
        "phaser" => rgb(234, 174, 147),
        "delay" => rgb(217, 204, 186),
        _ => rgb(182, 174, 216),
    }
}

/// Width of a small pedal at this pedal's height: concept measurements are fractions of it.
fn unit(b: Rect) -> f32 {
    b.h * 200.0 / 360.0
}

fn knob_size(b: Rect) -> f32 {
    unit(b) * 0.24
}

/// Knobs in two rows: small pedals 2 per row (odd one centred), wide pedals 4 + 3.
fn knob_center(sp: &EffectSpec, b: Rect, k: usize) -> (f32, f32) {
    let n = sp.params.len();
    let per_row = if sp.wide { 4 } else { 2 };
    let (row, col) = (k / per_row, k % per_row);
    let in_row = per_row.min(n - row * per_row);
    let cx = if sp.wide {
        b.x + b.w * (0.5 + (col as f32 - (in_row as f32 - 1.0) / 2.0) * 0.2)
    } else {
        b.x + b.w * if in_row == 1 { 0.5 } else if col == 0 { 0.28 } else { 0.72 }
    };
    (cx, b.y + b.h * (0.095 + row as f32 * 0.18))
}

struct PedalLayout {
    bodies: Vec<Rect>,
    scale: f32,
    divider_y: f32,
}

fn pedal_layout(specs: &[EffectSpec], area: Rect) -> PedalLayout {
    // concept proportions: a small pedal is ~12% of the page width with ~0.9 pedal-widths between
    // pedals; the five-pedal effects page tightens the gaps so its pedals stay about the same size
    let (base_h, small_w, wide_w, top, bottom) = (360.0, 200.0, 390.0, 100.0, 124.0);
    let gap = if specs.len() > 4 { 120.0 } else { 176.0 };
    let total: f32 = specs.iter().map(|e| if e.wide { wide_w } else { small_w }).sum::<f32>() + gap * (specs.len() - 1) as f32;
    let s = ((area.w - 100.0) / total).min((area.h - top - bottom) / base_h).min(0.117 * area.w / small_w).clamp(0.45, 1.45);
    let mut x = area.x + (area.w - total * s) / 2.0;
    let y = area.y + top + (area.h - top - bottom - base_h * s) / 2.0;
    let bodies = specs
        .iter()
        .map(|e| {
            let w = if e.wide { wide_w } else { small_w } * s;
            let b = Rect::new(x, y, w, base_h * s);
            x += w + gap * s;
            b
        })
        .collect();
    PedalLayout { bodies, scale: s, divider_y: y + base_h * s + 30.0 * s }
}

/// The enclosure: chrome ports cut from the concept, then the pastel body over them.
fn enclosure(app: &App, p: &mut Painter, sp: &EffectSpec, b: Rect) {
    let u = unit(b);
    let (top, left, right) = (app.tex("port_top.png"), app.tex("port_left.png"), app.tex("port_right.png"));
    let sz = |t: TexId| p.tex_sizes[t.0];
    let (tw0, th0) = sz(top);
    let (tw, th) = (u * 0.17, u * 0.17 * th0 as f32 / tw0 as f32);
    p.image(top, Rect::new(b.x + u * 0.16, b.y - th + 1.0, tw, th));
    p.image(top, Rect::new(b.right() - u * 0.15 - tw, b.y - th + 1.0, tw, th));
    let py = b.y + b.h * 0.44;
    let (lw0, lh0) = sz(left);
    let (lw, lh) = (u * 0.069, u * 0.069 * lh0 as f32 / lw0 as f32);
    p.image(left, Rect::new(b.x - lw + 1.0, py - lh / 2.0, lw, lh));
    let (rw0, rh0) = sz(right);
    let (rw, rh) = (u * 0.064, u * 0.064 * rh0 as f32 / rw0 as f32);
    p.image(right, Rect::new(b.right() - 1.0, py - rh / 2.0, rw, rh));
    p.image(app.tex(&format!("pastel_{}.png", sp.id)), b);
}

fn footswitch_rect(sp: &EffectSpec, b: Rect, second: bool) -> Rect {
    let u = unit(b);
    let fs = u * 0.29;
    let cx = if sp.has_tap { b.x + b.w * if second { 0.7 } else { 0.3 } } else { b.cx() };
    Rect::centered(cx, b.y + b.h * 0.755, fs, fs)
}

fn led_rect(sp: &EffectSpec, b: Rect, second: bool) -> Rect {
    let u = unit(b);
    let s = u * 0.085;
    let cx = if sp.has_tap { b.x + b.w * if second { 0.7 } else { 0.3 } } else { b.cx() };
    Rect::centered(cx, b.y + b.h * 0.615, s, s)
}

fn pedal_page(app: &mut App, p: &mut Painter, area: Rect, post: bool) {
    let specs: &[EffectSpec] = if post { &spec::POST } else { &spec::PRE };
    let (title, subtitle) = if post {
        ("POST EFFECTS", "Stereo doubler, modulation, delay and reverb after the cab — delay and reverb tails ring out when switched off")
    } else {
        ("PRE-AMP PEDALS", "Dynamics and drive in front of the amp — click a footswitch to engage, drag knobs (Shift = fine, double-click = reset)")
    };
    let lay = pedal_layout(specs, area);
    let s = lay.scale;

    p.fill(area, SLATE);
    p.text(Font::Semibold, 15.0, title, 30.0, area.y + 20.0, rgb(240, 242, 245));
    p.text(Font::Regular, 9.0, subtitle, 33.0, area.y + 52.0, rgba(205, 210, 220, 150));

    // GPU soft shadows (light from the upper left: twice as far right as down), cached per layout:
    // the pedals with their ports on the slate, then knobs and footswitches on the pedal lids
    let u = unit(lay.bodies[0]);
    let key = layout_key(area, post as u64);
    let bodies = lay.bodies.clone();
    p.shadow(
        0,
        key,
        [
            ShadowTerm { dx: u * 0.024, dy: u * 0.012, blur: u * 0.015, strength: 0.6 },
            ShadowTerm { dx: u * 0.14, dy: u * 0.07, blur: u * 0.1, strength: 0.5 },
            ShadowTerm { dx: u * 0.32, dy: u * 0.16, blur: u * 0.32, strength: 0.4 },
        ],
        |op| {
            for (sp, b) in specs.iter().zip(&bodies) {
                enclosure(app, op, sp, *b);
            }
        },
        None,
    );
    for (sp, b) in specs.iter().zip(&lay.bodies) {
        enclosure(app, p, sp, *b);
    }
    let parts = |op: &mut Painter| {
        for (sp, b) in specs.iter().zip(&bodies) {
            let ks = knob_size(*b);
            for k in 0..sp.params.len() {
                let (cx, cy) = knob_center(sp, *b, k);
                op.image(app.tex(&format!("knob_{}.png", sp.id)), Rect::centered(cx, cy, ks, ks));
            }
            let fs = app.tex(&format!("footswitch_{}.png", sp.id));
            op.image(fs, footswitch_rect(sp, *b, false));
            if sp.has_tap {
                op.image(fs, footswitch_rect(sp, *b, true));
            }
        }
    };
    let lids = |op: &mut Painter| {
        for (sp, b) in specs.iter().zip(&bodies) {
            op.image(app.tex(&format!("pastel_{}.png", sp.id)), *b);
        }
    };
    p.shadow(
        1,
        key,
        [
            ShadowTerm { dx: u * 0.03, dy: u * 0.015, blur: u * 0.012, strength: 0.75 },
            ShadowTerm { dx: u * 0.14, dy: u * 0.07, blur: u * 0.06, strength: 0.65 },
            ShadowTerm { dx: u * 0.24, dy: u * 0.12, blur: u * 0.16, strength: 0.4 },
        ],
        parts,
        Some(&lids),
    );

    for (i, (sp, b)) in specs.iter().zip(&lay.bodies).enumerate() {
        let b = *b;
        let params = app.params.clone();
        let fx = if post { &params.post[i] } else { &params.pre[i] };
        let on = fx.on.value();
        print(p, sp, b, s);

        let ks = knob_size(b);
        let img = app.tex(&format!("knob_{}.png", sp.id));
        for k in 0..sp.params.len() {
            let (cx, cy) = knob_center(sp, b, k);
            let kn = if post { Knob::Post(i, k) } else { Knob::Pre(i, k) };
            knob(app, p, Rect::centered(cx, cy, ks, ks), kn, img, KnobLook { ticks: None, baked_shadow: true });
        }

        let fs_img = app.tex(&format!("footswitch_{}.png", sp.id));
        let dot = app.tex(&format!("led_off_{}.png", sp.id));
        let stomp = Hit::Stomp(post, i);
        let fr = footswitch_rect(sp, b, false);
        p.image(fs_img, if pressed(app, stomp) { fr.inflate(-2.0, -2.0) } else { fr });
        app.hits.push((fr.inflate(10.0, 10.0), stomp));
        led_glow(app, p, led_rect(sp, b, false), on, dot, None);
        if sp.has_tap {
            let tr = footswitch_rect(sp, b, true);
            p.image(fs_img, if pressed(app, Hit::Tap) { tr.inflate(-2.0, -2.0) } else { tr });
            app.hits.push((tr.inflate(10.0, 10.0), Hit::Tap));
            let period = spec::delay_time_ms(fx.knobs[0].value()).max(1.0) as u128;
            let blink = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() % period < 70).unwrap_or(false);
            led_glow(app, p, led_rect(sp, b, true), blink, dot, Some(rgb(120, 255, 150)));
        }
    }

    // divider and caption row
    let r = area;
    p.fill(Rect::new(40.0, lay.divider_y, r.w - 80.0, 1.0), rgba(255, 255, 255, 40));
    let mut slots = vec![0.0; specs.len()];
    for i in 0..specs.len() {
        let cx = lay.bodies[i].cx();
        let left = if i > 0 { cx - lay.bodies[i - 1].cx() } else { 2.0 * (cx - 20.0) };
        let right = if i + 1 < specs.len() { lay.bodies[i + 1].cx() - cx } else { 2.0 * (r.w - 20.0 - cx) };
        slots[i] = left.min(right) * 0.92;
    }
    let mut cs = (r.w / 80.0).clamp(11.0, 22.0);
    for (i, sp) in specs.iter().enumerate() {
        cs = cs.min(20.0 * slots[i] / text::measure(Font::Bold, 20.0, sp.caption));
    }
    let cs = cs.max(6.0);
    let ch = text::line_metrics(Font::Bold, cs).height() * 1.1;
    for (i, sp) in specs.iter().enumerate() {
        let b = lay.bodies[i];
        let on = if post { app.params.post[i].on.value() } else { app.params.pre[i].on.value() };
        let cr = Rect::new(b.cx() - slots[i] / 2.0, lay.divider_y + 12.0, slots[i], ch);
        p.text_in(Font::Bold, cs, sp.caption, cr, 0.5, 0.5, if on { rgb(245, 246, 248) } else { rgba(245, 246, 248, 105) });
        if on {
            let uw = (b.w * 0.3).min(46.0 * s);
            p.rrect(Rect::new(b.cx() - uw / 2.0, cr.bottom() + 4.0 * s, uw, (3.0 * s).max(2.0)), 1.5 * s, accent(sp.id));
        }
    }
}

/// Text printed on the lid: knob labels, the pedal name, ON/TAP.
fn print(p: &mut Painter, sp: &EffectSpec, b: Rect, s: f32) {
    let u = unit(b);
    let kf = (6.75 * s).max(5.6);
    let ks = knob_size(b);
    // drawn at 80% size but letter-spaced out to the width the label has at full size
    for (k, ps) in sp.params.iter().enumerate() {
        let (cx, cy) = knob_center(sp, b, k);
        let width = text::measure(Font::Semibold, kf, ps.name);
        let top = cy + ks / 2.0 + (text::line_metrics(Font::Semibold, kf).height() - text::line_metrics(Font::Semibold, kf * 0.8).height()) / 2.0 + kf * 0.75;
        p.text_spaced(Font::Semibold, kf * 0.8, ps.name, cx - width / 2.0, top, width, INK);
    }
    let _ = u;

    // the pedal name: one bold line, or two lines with the top word in the regular weight. Every pedal
    // on both pages uses GREEN DRIVE's size (two lines fitted to a small pedal), so the names match;
    // a name only shrinks below it if it couldn't otherwise fit its own pedal.
    let words: Vec<&str> = sp.name.splitn(2, ' ').collect();
    let name_r = Rect::new(b.x + b.w * 0.08, b.y + b.h * 0.405, b.w * 0.84, b.h * 0.155);
    let fonts: Vec<Font> = if words.len() == 2 { vec![Font::Regular, Font::Bold] } else { vec![Font::Bold] };
    let mut nf = pedal_name_size(b);
    for (w, f) in words.iter().zip(&fonts) {
        nf = nf.min(20.0 * name_r.w / text::measure(*f, 20.0, w));
    }
    let nf = nf.max(8.0);
    let lh = text::line_metrics(Font::Bold, nf).height() * 0.9;
    let mut ny = name_r.y + (name_r.h - lh * words.len() as f32) / 2.0;
    for (w, f) in words.iter().zip(&fonts) {
        p.text_in(*f, nf, w, Rect::new(b.x, ny, b.w, lh), 0.5, 0.5, INK);
        ny += lh;
    }

    if sp.has_tap {
        let lf = (7.5 * s).max(6.0);
        let ly = b.y + b.h * 0.615;
        p.text_in(Font::Bold, lf, "ON", Rect::new(b.x + b.w * 0.3 + 10.0 * s, ly - lf, 60.0, lf * 2.0), 0.0, 0.5, INK);
        p.text_in(Font::Bold, lf, "TAP", Rect::new(b.x + b.w * 0.7 + 10.0 * s, ly - lf, 60.0, lf * 2.0), 0.0, 0.5, INK);
    }
}

/// Font size of "GREEN DRIVE" (two lines) fitted to a small pedal of this height: the size every
/// pedal name uses.
fn pedal_name_size(b: Rect) -> f32 {
    let s = b.h / 360.0;
    let small_w = unit(b);
    let name_w = small_w * 0.84;
    let name_h = b.h * 0.155;
    let lh_unit = text::line_metrics(Font::Bold, 1.0).height();
    let mut nf = (28.0 * s).min(name_h / (2.0 * lh_unit * 0.9));
    for (w, f) in [("GREEN", Font::Regular), ("DRIVE", Font::Bold)] {
        nf = nf.min(20.0 * name_w / text::measure(f, 20.0, w));
    }
    nf
}

/// The concept's LED: the unlit dot, or its glow (3x the LED size) when lit; `tint` recolours the glow.
fn led_glow(app: &App, p: &mut Painter, r: Rect, on: bool, off: TexId, tint: Option<Color>) {
    if !on {
        p.image(off, r);
        return;
    }
    let glow = app.tex("led_glow.png");
    p.image_tinted(glow, r.inflate(r.w, r.h), tint.unwrap_or([1.0; 4]));
}

fn layout_key(area: Rect, extra: u64) -> u64 {
    ((area.w as u64) << 40) ^ ((area.h as u64) << 20) ^ ((area.y as u64) << 8) ^ extra
}

// ===================================================================== amp page

fn tolex_box(app: &App, p: &mut Painter, r: Rect, scale: f32) {
    box_shadow(p, r, 18.0, 160, 6.0, 10.0);
    p.tile(app.tex("tolex.jpg"), r, scale, 18.0);
    p.grad(r, 18.0, rgba(255, 255, 255, 50), rgba(0, 0, 0, 90));
    p.rrect_stroke(r.inflate(-8.0, -8.0), 12.0, 1.5, with_alpha(GOLD, 160));
    for (cx, cy) in [(r.x, r.y), (r.right() - 30.0, r.y), (r.x, r.bottom() - 30.0), (r.right() - 30.0, r.bottom() - 30.0)] {
        let c = Rect::new(cx, cy, 30.0, 30.0);
        p.grad(c, 9.0, rgb(230, 230, 225), rgb(90, 90, 95));
        p.ellipse(Rect::new(cx + 12.0, cy + 12.0, 6.0, 6.0), rgb(60, 60, 60));
    }
}

// Control positions on the amp head as fractions of amp_head.png (measured by tools/cut_amp.py on the
// Qwen Image 2.1 render made from concepts/rect_C_2.png; knobs since packed closer to make room for
// the acoustic switches).
const AMP_KNOB_X: [f32; 7] = [0.2751, 0.3501, 0.4251, 0.5001, 0.5751, 0.6501, 0.7251];
const AMP_ROW_Y: f32 = 0.4131;
const AMP_KNOB_D: f32 = 0.056;
const AMP_LED_D: f32 = 0.019;
/// CLEAN, CRUNCH, LEAD, ACOUSTIC on the left; the acoustic channel's DECAY, ATTACK and
/// DYNAMIC switches, BRIGHT and POWER on the right.
const AMP_CH_X: [f32; 4] = [0.092, 0.137, 0.182, 0.227];
const AMP_ACOUSTIC_X: [f32; 3] = [0.785, 0.821, 0.857];
const AMP_BRIGHT_X: f32 = 0.899;
const AMP_POWER_X: f32 = 0.937;
/// Cabinet width relative to the head in the stack render.
const AMP_CAB_W: f32 = 1.15;

struct AmpLayout {
    head: Rect,
    cab: Rect,
}

fn amp_layout(app: &App, r: Rect) -> AmpLayout {
    let (hw0, hh0) = app.tex_size("amp_head.png");
    let (cw0, ch0) = app.tex_size("amp_cab.png");
    // the head as wide as fits (the cab is 15% wider), leaving room for the title above
    let hw = ((r.w - 100.0) / AMP_CAB_W).min(1400.0).max(600.0);
    let hh = hw * hh0 / hw0;
    let head = Rect::new(r.x + (r.w - hw) / 2.0, r.y + 96.0, hw, hh);
    let cw = hw * AMP_CAB_W;
    let cab = Rect::new(r.x + (r.w - cw) / 2.0, head.bottom() - hh * 0.005, cw, cw * ch0 / cw0);
    AmpLayout { head, cab }
}

/// A lit or unlit panel LED from the render; `glow` tints the lit lamp's bloom.
fn amp_led(app: &App, p: &mut Painter, c: (f32, f32), d: f32, lit: Option<(&str, Color)>) {
    let rr = Rect::centered(c.0, c.1, d, d);
    match lit {
        Some((img, glow)) => {
            p.glow(rr.inflate(d * 1.4, d * 1.4), with_alpha(glow, 120), with_alpha(glow, 0), 0.15);
            p.image(app.tex(img), rr);
        }
        None => p.image(app.tex("amp_led_off.png"), rr),
    }
}

fn amp_page(app: &mut App, p: &mut Painter, r: Rect) {
    const PRINT: Color = [232.0 / 255.0, 230.0 / 255.0, 222.0 / 255.0, 0.92];
    let l = amp_layout(app, r);
    let (head, cab) = (l.head, l.cab);
    let hw = head.w;
    let at = |fx: f32, fy: f32| (head.x + head.w * fx, head.y + head.h * fy);

    p.fill(r, SLATE);
    p.text(Font::Semibold, 15.0, "AMPLIFIER", 30.0, r.y + 20.0, rgb(240, 242, 245));
    p.text(Font::Regular, 9.0, "Three tube channels plus an acoustic-guitar simulator — click a channel LED to switch, drag knobs (Shift = fine, double-click = reset)", 33.0, r.y + 52.0, rgba(205, 210, 220, 150));

    // GPU soft shadows like the pedal pages: the stack on the slate, then the knobs and LEDs on the faceplate
    let u = hw / 1.6;
    let key = layout_key(r, 7);
    let (head_t, cab_t) = (app.tex("amp_head.png"), app.tex("amp_cab.png"));
    p.shadow(
        2,
        key,
        [
            ShadowTerm { dx: u * 0.012, dy: u * 0.006, blur: u * 0.01, strength: 0.6 },
            ShadowTerm { dx: u * 0.07, dy: u * 0.035, blur: u * 0.05, strength: 0.5 },
            ShadowTerm { dx: u * 0.16, dy: u * 0.08, blur: u * 0.16, strength: 0.4 },
        ],
        |op| {
            op.image(cab_t, cab);
            op.image(head_t, head);
        },
        None,
    );
    p.image(cab_t, cab);
    p.image(head_t, head);

    let kd = hw * AMP_KNOB_D;
    let ld = hw * AMP_LED_D;
    let knob_t = app.tex("amp_knob.png");
    let parts = |op: &mut Painter| {
        for fx in AMP_KNOB_X {
            let c = at(fx, AMP_ROW_Y);
            op.image(knob_t, Rect::centered(c.0, c.1, kd, kd));
        }
        for fx in AMP_CH_X.iter().chain(AMP_ACOUSTIC_X.iter()).chain([AMP_BRIGHT_X, AMP_POWER_X].iter()) {
            let c = at(*fx, AMP_ROW_Y);
            op.ellipse(Rect::centered(c.0, c.1, ld * 1.1, ld * 1.1), [0.0, 0.0, 0.0, 1.0]);
        }
    };
    let face = |op: &mut Painter| op.image(head_t, head);
    p.shadow(
        3,
        key,
        [
            ShadowTerm { dx: kd * 0.05, dy: kd * 0.025, blur: kd * 0.04, strength: 0.7 },
            ShadowTerm { dx: kd * 0.2, dy: kd * 0.1, blur: kd * 0.12, strength: 0.55 },
            ShadowTerm { dx: kd * 0.4, dy: kd * 0.2, blur: kd * 0.3, strength: 0.35 },
        ],
        parts,
        Some(&face),
    );

    // printing on the faceplate: labels above the controls, white like the real thing
    let lf = (hw / 150.0).clamp(6.0, 9.5);
    let label_y = head.y + head.h * AMP_ROW_Y - kd * 0.5 - lf * 2.6;
    let label = |p: &mut Painter, x: f32, s: &str| p.text_in(Font::ArialBold, lf, s, Rect::new(x - 60.0, label_y, 120.0, lf * 2.0), 0.5, 0.5, PRINT);

    // seven knobs with scale ticks
    for (i, fx) in AMP_KNOB_X.iter().enumerate() {
        let c = at(*fx, AMP_ROW_Y);
        knob(app, p, Rect::centered(c.0, c.1, kd, kd), Knob::Amp(i), knob_t, KnobLook { ticks: Some(with_alpha(PRINT, 150)), baked_shadow: true });
        let names = if app.params.channel.value() == spec::ACOUSTIC { &spec::ACOUSTIC_KNOB_NAMES } else { &spec::AMP_KNOB_NAMES_SHORT };
        label(p, c.0, names[i]);
    }

    // channel LEDs: lit = the active channel
    let ch = app.params.channel.value();
    let lit = [
        ("amp_led_green.png", rgb(60, 230, 110)),
        ("amp_led_red.png", rgb(255, 170, 40)),
        ("amp_led_red.png", rgb(255, 50, 40)),
        ("amp_led_green.png", rgb(90, 180, 255)),
    ];
    for (i, fx) in AMP_CH_X.iter().enumerate() {
        let c = at(*fx, AMP_ROW_Y);
        let on = ch == i as i32;
        amp_led(app, p, c, ld, on.then_some(lit[i]));
        if on && i == 1 {
            // crunch is amber: warm the red lamp's core
            p.ellipse(Rect::centered(c.0, c.1, ld * 0.55, ld * 0.55), rgba(255, 190, 70, 210));
        }
        if on && i == 3 {
            // acoustic is blue: cool the green lamp's core
            p.ellipse(Rect::centered(c.0, c.1, ld * 0.55, ld * 0.55), rgba(130, 200, 255, 210));
        }
        p.text_in(Font::ArialBold, lf * 0.82, spec::CHANNELS[i], Rect::new(c.0 - 40.0, c.1 + ld * 0.8, 80.0, lf * 2.0), 0.5, 0.5, PRINT);
        let hit = Rect::centered(c.0, c.1 + ld * 0.6, (hw * 0.04).max(ld * 2.2), ld * 3.2);
        if hot(app, Hit::Channel(i as u8)) {
            p.ellipse_stroke(Rect::centered(c.0, c.1, ld * 1.7, ld * 1.7), 1.2, with_alpha(PRINT, 120));
        }
        app.hits.push((hit, Hit::Channel(i as u8)));
    }
    let c0 = at(AMP_CH_X[0], AMP_ROW_Y);
    let c3 = at(AMP_CH_X[3], AMP_ROW_Y);
    p.text_in(Font::ArialBold, lf, "CHANNEL", Rect::new(c0.0, label_y, c3.0 - c0.0, lf * 2.0), 0.5, 0.5, PRINT);

    // the acoustic channel's switches: dimmed print while another channel is selected
    let acoustic = ch == spec::ACOUSTIC;
    let ink = if acoustic { PRINT } else { with_alpha(PRINT, 110) };
    let sw = [app.params.decay.value(), app.params.attack.value(), app.params.dynamic.value()];
    for (i, fx) in AMP_ACOUSTIC_X.iter().enumerate() {
        let c = at(*fx, AMP_ROW_Y);
        amp_led(app, p, c, ld, sw[i].then_some(("amp_led_green.png", rgb(90, 180, 255))));
        if sw[i] {
            // blue like the acoustic channel's lamp
            p.ellipse(Rect::centered(c.0, c.1, ld * 0.55, ld * 0.55), rgba(130, 200, 255, 210));
        }
        p.text_in(Font::ArialBold, lf * 0.82, spec::ACOUSTIC_SWITCHES[i], Rect::new(c.0 - 40.0, c.1 + ld * 0.8, 80.0, lf * 2.0), 0.5, 0.5, ink);
        if hot(app, Hit::Acoustic(i as u8)) {
            p.ellipse_stroke(Rect::centered(c.0, c.1, ld * 1.7, ld * 1.7), 1.2, with_alpha(PRINT, 120));
        }
        app.hits.push((Rect::centered(c.0, c.1 + ld * 0.6, ld * 1.8, ld * 3.2), Hit::Acoustic(i as u8)));
    }
    let a0 = at(AMP_ACOUSTIC_X[0], AMP_ROW_Y);
    let a2 = at(AMP_ACOUSTIC_X[2], AMP_ROW_Y);
    p.text_in(Font::ArialBold, lf, "ACOUSTIC", Rect::new(a0.0 - 20.0, label_y, a2.0 - a0.0 + 40.0, lf * 2.0), 0.5, 0.5, ink);

    // bright
    let bright = app.params.bright.value();
    let c = at(AMP_BRIGHT_X, AMP_ROW_Y);
    amp_led(app, p, c, ld, bright.then_some(("amp_led_red.png", rgb(255, 50, 40))));
    p.text_in(Font::ArialBold, lf * 0.82, "BRIGHT", Rect::new(c.0 - 40.0, c.1 + ld * 0.8, 80.0, lf * 2.0), 0.5, 0.5, PRINT);
    if hot(app, Hit::Bright) {
        p.ellipse_stroke(Rect::centered(c.0, c.1, ld * 1.7, ld * 1.7), 1.2, with_alpha(PRINT, 120));
    }
    app.hits.push((Rect::centered(c.0, c.1 + ld * 0.6, ld * 1.8, ld * 3.2), Hit::Bright));

    // power: the audio switch in the standalone; always lit in a plugin, where the host runs audio
    let on = app.audio.is_none() || app.audio_running();
    let c = at(AMP_POWER_X, AMP_ROW_Y);
    amp_led(app, p, c, ld, on.then_some(("amp_led_green.png", rgb(60, 230, 110))));
    p.text_in(Font::ArialBold, lf * 0.82, "POWER", Rect::new(c.0 - 40.0, c.1 + ld * 0.8, 80.0, lf * 2.0), 0.5, 0.5, PRINT);
    if app.audio.is_some() {
        if hot(app, Hit::Power) {
            p.ellipse_stroke(Rect::centered(c.0, c.1, ld * 1.7, ld * 1.7), 1.2, with_alpha(PRINT, 120));
        }
        app.hits.push((Rect::centered(c.0, c.1 + ld * 0.6, ld * 1.8, ld * 3.2), Hit::Power));
    }
}

// ===================================================================== cab page

struct CabLayout {
    bx: Rect,
    panel: Rect,
    spk: (f32, f32),
    spk_r: f32,
}

fn cab_layout(r: Rect) -> CabLayout {
    let size = (r.h - 70.0).min(r.w * 0.5);
    let pw = 380.0;
    let total = size + 50.0 + pw;
    let (x0, y0) = (r.x + (r.w - total) / 2.0, r.y + (r.h - size) / 2.0);
    let bx = Rect::new(x0, y0, size, size);
    CabLayout { bx, panel: Rect::new(bx.right() + 50.0, y0, pw, size), spk: (bx.cx(), bx.cy()), spk_r: size * 0.38 }
}

/// Mic position for a drag to `x` (cap at the centre, cone towards the edge).
pub fn mic_value(app: &App, x: f32) -> Option<f32> {
    let (w, h) = app.size();
    let l = cab_layout(Rect::new(0.0, HEADER, w, h - HEADER));
    Some(((x - l.spk.0) / (l.spk_r * 0.92)).clamp(0.0, 1.0))
}

fn cab_page(app: &mut App, p: &mut Painter, r: Rect) {
    let l = cab_layout(r);
    let (bx, panel) = (l.bx, l.panel);
    p.cover(app.tex("backdrop.jpg"), r);
    p.fill(r, rgba(0, 0, 0, 140));
    vignette(p, r, 200);

    // cabinet with the grille cloth removed so the speaker is visible
    box_shadow(p, bx, 16.0, 170, 6.0, 10.0);
    p.tile(app.tex("tolex.jpg"), bx, 0.5, 16.0);
    let baffle = bx.inflate(-28.0, -28.0);
    p.grad(baffle, 0.0, rgb(28, 26, 24), rgb(10, 9, 8));
    p.rrect_stroke(baffle, 0.0, 2.0, GOLD);
    let sr = Rect::centered(l.spk.0, l.spk.1, l.spk_r * 2.24, l.spk_r * 2.24);
    p.ellipse(sr.inflate(6.0, 6.0), rgba(0, 0, 0, 160));
    p.image(app.tex("speaker.png"), sr);
    p.logo(Rect::new(baffle.x + 10.0, baffle.y + 4.0, 260.0, 44.0), 16.0, 0.0);

    // the mic: drag across the speaker from the dust cap (centre) to the cone edge
    let (pos, dist) = (app.params.cab[0].value(), app.params.cab[1].value());
    let ms = l.spk_r * (0.42 - 0.16 * dist);
    let mx = l.spk.0 + pos * l.spk_r * 0.92;
    let mic = Rect::centered(mx, l.spk.1, ms, ms);
    let arm_end = (bx.right() - 6.0, bx.bottom() - 6.0);
    p.line(mic.cx() + 5.0, mic.cy() + 8.0, arm_end.0 + 5.0, arm_end.1 + 8.0, ms * 0.16 + 4.0, rgba(0, 0, 0, 90));
    p.line(mic.cx(), mic.cy(), arm_end.0, arm_end.1, ms * 0.16, rgb(40, 40, 44));
    p.ellipse(Rect::new(mic.x + 6.0, mic.y + 10.0, mic.w, mic.h), rgba(0, 0, 0, 120));
    p.image(app.tex("mic.png"), mic);
    if hot(app, Hit::Mic) || pressed(app, Hit::Mic) {
        p.ellipse_stroke(mic.inflate(4.0, 4.0), 2.0, with_alpha(GOLD, 180));
    }
    let lbl = format!("{} · {}", (spec::CAB_KNOBS[0].fmt)(pos), (spec::CAB_KNOBS[1].fmt)(dist));
    p.text_in(Font::Bold, 8.5, &lbl, Rect::new(bx.x, bx.bottom() - 30.0, bx.w, 20.0), 0.5, 0.5, GOLD);
    app.hits.push((Rect::new(l.spk.0 - l.spk_r, l.spk.1 - ms / 2.0 - 10.0, l.spk_r * 2.0, ms + 20.0), Hit::Mic));

    // control panel
    box_shadow(p, panel, 12.0, 140, 6.0, 10.0);
    p.grad(panel, 12.0, rgba(36, 32, 28, 235), rgba(18, 16, 14, 235));
    p.rrect_stroke(panel, 12.0, 1.0, with_alpha(GOLD, 90));
    p.text(Font::Bold, 14.0, "CABINET", panel.x + 18.0, panel.y + 20.0, GOLD);
    if app.params.channel.value() == spec::ACOUSTIC {
        p.text(Font::Bold, 8.0, "SPEAKER MODEL — bypassed on the acoustic channel", panel.x + 22.0, panel.y + 64.0, DIM);
    } else {
        p.text(Font::Bold, 8.0, "SPEAKER MODEL", panel.x + 22.0, panel.y + 64.0, DIM);
    }
    p.text(Font::Bold, 8.0, "MICROPHONE — drag the mic across the speaker", panel.x + 22.0, panel.y + 226.0, DIM);
    let cab_on = app.params.cab_on.value();
    switch(app, p, Rect::new(panel.right() - 104.0, panel.y + 22.0, 84.0, 28.0), "CAB ON", Hit::CabOn, cab_on, rgb(80, 180, 255));
    let model = app.params.cab_model.value();
    for i in 0..4u8 {
        let br = Rect::new(panel.x + 20.0 + (i % 2) as f32 * 172.0, panel.y + 86.0 + (i / 2) as f32 * 40.0, 164.0, 32.0);
        button(app, p, br, spec::CAB_MODELS[i as usize], Hit::CabModel(i), model == i as i32, 8.5);
    }
    button(app, p, Rect::new(panel.x + 20.0, panel.y + 176.0, 120.0, 30.0), "LOAD IR…", Hit::LoadIr, false, 8.5);
    let ir_name = app
        .shared
        .user_ir
        .lock()
        .unwrap()
        .as_ref()
        .map(|u| std::path::Path::new(&u.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
        .unwrap_or_else(|| "no IR loaded".into());
    p.text_fit(Font::Regular, 8.5, &ir_name, Rect::new(panel.x + 150.0, panel.y + 176.0, panel.w - 170.0, 30.0), 0.0, DIM);

    let (ks, gx) = (66.0, (panel.w - 40.0) / 2.0);
    let img = app.tex("knob.png");
    for i in 0..4 {
        let cx = panel.x + 20.0 + gx * (i % 2) as f32 + gx / 2.0;
        let mut cy = panel.y + 322.0 + (i / 2) as f32 * 124.0;
        if cy + ks / 2.0 > panel.bottom() - 30.0 {
            cy = panel.bottom() - 30.0 - ks / 2.0;
        }
        let kr = Rect::centered(cx, cy, ks, ks);
        knob(app, p, kr, Knob::Cab(i), img, KnobLook { ticks: Some(rgba(214, 178, 96, 150)), baked_shadow: false });
        p.text_in(Font::ArialBold, 7.5, spec::CAB_KNOBS[i].name, Rect::new(kr.x - 30.0, kr.y - 26.0, kr.w + 60.0, 14.0), 0.5, 0.5, TEXT);
    }
}

// ===================================================================== graphic EQ page

/// Height of an EQ slider's thumb.
pub const FADER_THUMB_H: f32 = 20.0;
/// The response display spans ±this many dB.
const EQ_VIEW_DB: f32 = 18.0;

fn eq_page(app: &mut App, p: &mut Painter, r: Rect) {
    const AMBER: Color = [255.0 / 255.0, 186.0 / 255.0, 84.0 / 255.0, 1.0];
    p.cover(app.tex("backdrop.jpg"), r);
    p.fill(r, rgba(0, 0, 0, 140));
    vignette(p, r, 200);
    p.text(Font::Semibold, 15.0, "GRAPHIC EQ", 30.0, r.y + 20.0, rgb(240, 242, 245));
    p.text(Font::Regular, 9.0, "Nine octave bands after the cab, before the post effects — drag a slider (double-click = 0 dB, wheel = fine steps)", 33.0, r.y + 52.0, rgba(205, 210, 220, 150));

    let on = app.params.eq_on.value();
    let vals: Vec<f32> = app.params.eq.iter().map(|k| k.value()).collect();

    // the rack unit
    let uw = (r.w - 120.0).min(1080.0);
    let uh = (r.h - 130.0).min(560.0);
    let unit = Rect::new(r.x + (r.w - uw) / 2.0, r.y + 84.0 + ((r.h - 114.0 - uh) / 2.0).max(0.0), uw, uh);
    box_shadow(p, unit, 12.0, 150, 6.0, 10.0);
    p.grad(unit, 12.0, rgba(40, 36, 32, 240), rgba(18, 16, 14, 240));
    p.rrect_stroke(unit, 12.0, 1.0, with_alpha(GOLD, 90));
    p.text(Font::Bold, 14.0, "GRAPHIC EQ", unit.x + 22.0, unit.y + 18.0, GOLD);
    p.text(Font::Bold, 7.5, "9-BAND  ·  ±12 dB  ·  POST CAB", unit.x + 24.0, unit.y + 44.0, DIM);
    switch(app, p, Rect::new(unit.right() - 106.0, unit.y + 20.0, 84.0, 28.0), "EQ ON", Hit::EqOn, on, rgb(255, 150, 50));
    button(app, p, Rect::new(unit.right() - 182.0, unit.y + 20.0, 64.0, 28.0), "FLAT", Hit::EqFlat, false, 8.0);

    // response display: log frequency 20 Hz–20 kHz, ±18 dB
    let scr = Rect::new(unit.x + 22.0, unit.y + 66.0, unit.w - 44.0, (uh * 0.3).max(110.0));
    p.rrect(scr, 6.0, rgb(10, 11, 11));
    p.rrect_stroke(scr, 6.0, 1.0, with_alpha(GOLD, 60));
    let fx = |f: f64| scr.x + scr.w * ((f / 20.0).ln() / 1000f64.ln()) as f32;
    let dy = |d: f64| scr.cy() - (d as f32 / EQ_VIEW_DB) * (scr.h / 2.0 - 6.0);
    for d in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let y = dy(d);
        p.fill(Rect::new(scr.x + 1.0, y, scr.w - 2.0, 1.0), rgba(255, 255, 255, if d == 0.0 { 46 } else { 16 }));
        let lbl = if d == 0.0 { "0".to_string() } else { format!("{:+}", d) };
        p.text_in(Font::Regular, 6.5, &lbl, Rect::new(scr.x + 4.0, y - 10.0, 30.0, 10.0), 0.0, 1.0, rgba(255, 255, 255, 70));
    }
    for (i, f) in spec::EQ_FREQS.iter().enumerate() {
        let x = fx(*f);
        p.fill(Rect::new(x, scr.y + 1.0, 1.0, scr.h - 2.0), rgba(255, 255, 255, 14));
        p.text_in(Font::Regular, 6.5, spec::EQ_KNOBS[i].name, Rect::new(x - 20.0, scr.bottom() - 14.0, 40.0, 12.0), 0.5, 0.5, rgba(255, 255, 255, 70));
    }
    p.push_clip(scr.inflate(-1.0, -1.0));
    // at the running rate, so the top band's squeeze towards Nyquist shows as it sounds
    let fs = app.shared.sample_rate.load(Ordering::Relaxed) as f64;
    let fs = if fs > 0.0 { fs } else { 48000.0 };
    let n = 180;
    let pts: Vec<(f32, f32)> = (0..=n)
        .map(|k| {
            let f = 20.0 * 1000f64.powf(k as f64 / n as f64);
            (fx(f), dy(crate::dsp::eq::response_db(&vals, fs, f.min(0.499 * fs)).clamp(-40.0, 40.0)))
        })
        .collect();
    let c = if on { AMBER } else { with_alpha(DIM, 150) };
    for w in pts.windows(2) {
        if on {
            p.line(w[0].0, w[0].1, w[1].0, w[1].1, 7.0, with_alpha(AMBER, 34));
        }
        p.line(w[0].0, w[0].1, w[1].0, w[1].1, 2.0, c);
    }
    p.pop_clip();
    if !on {
        p.text_in(Font::Bold, 8.0, "BYPASSED", Rect::new(scr.right() - 110.0, scr.y + 8.0, 100.0, 14.0), 1.0, 0.5, DIM);
    }

    // sliders: nine bands, a gap, then LEVEL
    let top = scr.bottom() + 26.0;
    let bottom = unit.bottom() - 22.0;
    let (y0, y1) = (top + 24.0, bottom - 26.0);
    let left = unit.x + 64.0;
    let slots = spec::EQ_KNOBS.len() as f32 + 0.6;
    let sw = (unit.right() - 22.0 - left) / slots;
    let ty = |v: f32| y1 - v * (y1 - y0);
    for d in [-12.0f32, -6.0, 0.0, 6.0, 12.0] {
        let y = ty(0.5 + d / (2.0 * spec::EQ_RANGE_DB as f32));
        let lbl = if d == 0.0 { "0 dB".to_string() } else { format!("{:+}", d) };
        p.text_in(Font::ArialBold, 6.5, &lbl, Rect::new(unit.x + 18.0, y - 6.0, 38.0, 12.0), 1.0, 0.5, DIM);
    }
    let fader_hot = |app: &App, k: Knob| hot(app, Hit::Fader(k)) || pressed(app, Hit::Fader(k));
    for i in 0..spec::EQ_KNOBS.len() {
        let k = Knob::Eq(i);
        let level = i == spec::EQ_BANDS;
        let cx = left + sw * (i as f32 + 0.5 + if level { 0.6 } else { 0.0 });
        let v = vals[i];
        if level {
            let sx = cx - sw * 0.8;
            p.fill(Rect::new(sx, y0 - 8.0, 1.0, y1 - y0 + 16.0), rgba(255, 255, 255, 30));
        }
        // scale ticks
        for t in 0..=8 {
            let y = y0 + (y1 - y0) * t as f32 / 8.0;
            let half = if t == 4 { sw * 0.34 } else if t % 2 == 0 { sw * 0.26 } else { sw * 0.18 };
            p.fill(Rect::new(cx - half, y, half * 2.0, 1.0), rgba(255, 255, 255, if t == 4 { 70 } else { 26 }));
        }
        // the slot, lit from 0 dB to the thumb
        let slot = Rect::new(cx - 3.0, y0 - 4.0, 6.0, y1 - y0 + 8.0);
        p.rrect(slot, 3.0, rgb(4, 4, 4));
        p.rrect_stroke(slot, 3.0, 1.0, rgba(255, 255, 255, 22));
        let (ym, yt) = (ty(0.5), ty(v));
        if on && (yt - ym).abs() > 0.5 {
            p.rrect(Rect::new(cx - 2.0, ym.min(yt), 4.0, (yt - ym).abs()), 2.0, with_alpha(AMBER, 200));
        }
        // the thumb
        let tw = (sw * 0.6).clamp(26.0, 46.0);
        let th = FADER_THUMB_H;
        let thumb = Rect::centered(cx, yt, tw, th);
        p.rrect(Rect::new(thumb.x + 2.0, thumb.y + 4.0, tw, th), 4.0, rgba(0, 0, 0, 130));
        p.grad(thumb, 4.0, rgb(214, 208, 196), rgb(108, 102, 94));
        p.rrect_stroke(thumb, 4.0, 1.0, if fader_hot(app, k) { GOLD } else { rgba(0, 0, 0, 160) });
        p.fill(Rect::new(thumb.x + 4.0, thumb.cy() - 1.0, tw - 8.0, 2.0), if on { rgb(40, 30, 20) } else { rgb(70, 66, 60) });
        // value above, name below
        let val = (spec::EQ_KNOBS[i].fmt)(v);
        p.text_in(Font::Bold, 7.5, &val, Rect::new(cx - sw / 2.0, top - 4.0, sw, 14.0), 0.5, 0.5, if fader_hot(app, k) { GOLD } else { TEXT });
        let name = if level { "LEVEL".to_string() } else { format!("{} Hz", spec::EQ_KNOBS[i].name.replace('K', "k")) };
        p.text_in(Font::ArialBold, 7.0, &name, Rect::new(cx - sw / 2.0, bottom - 14.0, sw, 14.0), 0.5, 0.5, if level { GOLD } else { DIM });
        app.hits.push((Rect::new(cx - sw / 2.0, y0 - th / 2.0 - 2.0, sw, y1 - y0 + th + 4.0), Hit::Fader(k)));
        app.fader_tracks.push((k, Rect::new(cx - 1.0, y0, 2.0, y1 - y0)));
    }
}

// ===================================================================== tuner

const STRINGS: [(&str, f64); 6] = [("E2", 40.0), ("A2", 45.0), ("D3", 50.0), ("G3", 55.0), ("B3", 59.0), ("E4", 64.0)];

fn tuner(app: &mut App, p: &mut Painter, r: Rect, dt: f64) {
    let (in_tune, off, far) = (rgb(90, 235, 120), rgb(255, 176, 60), rgb(240, 90, 70));
    p.cover(app.tex("backdrop.jpg"), r);
    p.fill(r, rgba(6, 5, 4, 205));
    vignette(p, r, 200);
    p.text(Font::Bold, 15.0, "TUNER", 28.0, r.y + 18.0, with_alpha(GOLD, 220));
    p.text(Font::Regular, 9.0, "Play a single string and let it ring.  T or Esc closes the tuner.", 31.0, r.y + 48.0, rgba(230, 220, 200, 160));

    let w = (r.w - 80.0).min(900.0);
    let cx = r.w / 2.0;
    let readout = Rect::new(cx - w / 2.0, r.y + 30.0, w, 230.0);
    let meter = Rect::new(cx - w / 2.0, readout.bottom() + 20.0, w, 70.0);
    let strobe = Rect::new(cx - w / 2.0, meter.bottom() + 26.0, w, 34.0);
    let dim = rgba(230, 220, 200, 160);

    // scale
    for c in (-50..=50).step_by(5) {
        let x = meter.x + (c + 50) as f32 / 100.0 * meter.w;
        let h = if c % 25 == 0 { 22.0 } else if c % 10 == 0 { 15.0 } else { 9.0 };
        p.line(x, meter.bottom() - 22.0, x, meter.bottom() - 22.0 - h, 1.5, rgba(230, 220, 200, 120));
        if c % 25 == 0 {
            let s = if c > 0 { format!("+{c}") } else { c.to_string() };
            p.text_in(Font::Regular, 8.0, &s, Rect::new(x - 20.0, meter.bottom() - 18.0, 40.0, 16.0), 0.5, 0.5, dim);
        }
    }
    p.fill(Rect::new(meter.x + 0.47 * meter.w, meter.y, meter.w * 0.06, meter.h - 22.0), with_alpha(in_tune, 50));
    p.text(Font::Bold, 22.0, "♭", meter.x - 44.0, meter.y + 4.0, dim);
    p.text(Font::Bold, 22.0, "♯", meter.right() + 12.0, meter.y + 4.0, dim);
    p.rrect(strobe, 6.0, rgb(18, 16, 14));
    p.rrect_stroke(strobe, 6.0, 1.0, with_alpha(GOLD, 80));

    let sig = app.tuner.has_signal;
    let (note, oct, cents) = if sig { app.tuner.describe(app.tuner.frequency) } else { ("–", 0, 0.0) };
    let ac = cents.abs();
    let col = if !sig { rgba(230, 220, 200, 90) } else if ac < 3.0 { in_tune } else if ac < 15.0 { off } else { far };

    // note readout (120 px)
    let note_pt = 120.0 * 72.0 / 96.0;
    let nw = text::measure(Font::Bold, note_pt, note);
    let nx = readout.cx() - nw / 2.0;
    if sig && ac < 3.0 {
        p.glow(Rect::new(readout.cx() - 170.0, readout.y - 10.0, 340.0, 200.0), with_alpha(in_tune, 60), with_alpha(in_tune, 0), 0.0);
    }
    p.text(Font::Bold, note_pt, note, nx, readout.y, col);
    if sig {
        p.text(Font::Bold, 34.0 * 72.0 / 96.0, &oct.to_string(), nx + nw - 4.0, readout.y + 100.0, col);
    }
    let info = if sig { format!("{:.2} Hz      {}{:.1} ¢", app.tuner.frequency, if cents >= 0.0 { "+" } else { "" }, cents) } else { "Waiting for a note…".into() };
    p.text_in(Font::Bold, 13.0, &info, Rect::new(readout.x, readout.bottom() - 50.0, readout.w, 30.0), 0.5, 0.5, TEXT);

    // nearest standard-tuning string
    let midi = if sig { 69.0 + 12.0 * (app.tuner.frequency / app.tuner.reference_a).log2() } else { -100.0 };
    let mut nearest = None;
    let mut best = 1.5;
    for (i, (_, m)) in STRINGS.iter().enumerate() {
        if (midi - m).abs() < best {
            best = (midi - m).abs();
            nearest = Some(i);
        }
    }
    let sw = 58.0;
    let sx = readout.cx() - sw * STRINGS.len() as f32 / 2.0;
    for (i, (name, _)) in STRINGS.iter().enumerate() {
        let b = Rect::new(sx + i as f32 * sw + 4.0, readout.bottom() - 12.0, sw - 8.0, 24.0);
        let on = nearest == Some(i);
        p.rrect(b, 5.0, if on { with_alpha(col, 80) } else { rgba(255, 255, 255, 30) });
        p.text_in(Font::Bold, 9.5, name, b, 0.5, 0.5, if on { col } else { DIM });
    }

    // needle eased toward the target
    let target = if sig { cents.clamp(-50.0, 50.0) } else { 0.0 };
    app.needle += (target - app.needle) * (1.0 - (-dt * 12.0).exp());
    let nx2 = meter.x + ((app.needle + 50.0) / 100.0) as f32 * meter.w;
    p.line(nx2, meter.y, nx2, meter.bottom() - 24.0, 4.0, if sig { col } else { rgba(230, 220, 200, 60) });
    led(p, Rect::new(meter.x - 30.0, meter.y + 50.0, 10.0, 10.0), sig && cents < -3.0, off);
    led(p, Rect::new(meter.right() + 20.0, meter.y + 50.0, 10.0, 10.0), sig && cents > 3.0, off);

    // strobe: stripes drift left when flat, right when sharp, stand still in tune
    if sig {
        app.strobe_phase += cents * 2.2 * dt;
    }
    let period = 28.0;
    let mut o = (app.strobe_phase * period as f64 % period as f64) as f32;
    if o < 0.0 {
        o += period;
    }
    p.push_clip(strobe.inflate(-3.0, -3.0));
    let mut x = strobe.x - period + o;
    while x < strobe.right() {
        p.fill(Rect::new(x, strobe.y + 4.0, period / 2.0, strobe.h - 8.0), if sig { with_alpha(col, 200) } else { rgba(230, 220, 200, 40) });
        x += period;
    }
    p.pop_clip();

    let by = strobe.bottom() + 80.0;
    button(app, p, Rect::new(cx - 150.0, by, 34.0, 32.0), "◀", Hit::RefDown, false, 8.5);
    let ra = format!("A4 = {:.0} Hz", app.tuner.reference_a);
    p.text_in(Font::Bold, 10.0, &ra, Rect::new(cx - 110.0, by, 160.0, 32.0), 0.5, 0.5, TEXT);
    button(app, p, Rect::new(cx + 56.0, by, 34.0, 32.0), "▶", Hit::RefUp, false, 8.5);
    let mute = app.shared.tuner.mute_output.load(Ordering::Relaxed);
    switch(app, p, Rect::new(cx + 110.0, by + 2.0, 140.0, 28.0), "MUTE OUTPUT", Hit::TunerMute, mute, rgb(255, 80, 60));
    button(app, p, Rect::new(cx - 60.0, by + 52.0, 120.0, 34.0), "CLOSE TUNER", Hit::TunerClose, false, 8.5);
}

// ===================================================================== overlays

fn preset_menu(app: &mut App, p: &mut Painter) {
    let Some(menu) = &app.menu else { return };
    let (w, h) = app.size();
    let cx = (270.0 + w - 300.0) / 2.0 - 40.0;
    let row = 26.0;
    let is_user = presets::user_names().contains(&app.preset_name);
    let max_rows = (((h - 60.0) / row) as usize).max(4);
    let shown: Vec<(usize, String)> = menu.items.iter().cloned().enumerate().skip(menu.scroll).take(max_rows).collect();
    let user_from = menu.user_from;
    let headers = 1 + (user_from < menu.items.len() && menu.scroll <= user_from) as usize;
    let extra = if is_user { row + 6.0 } else { 0.0 };
    let r = Rect::new(cx - 152.0, 52.0, 304.0, (shown.len() + headers) as f32 * row + 8.0 + extra);
    p.rrect(Rect::new(r.x + 4.0, r.y + 6.0, r.w, r.h), 8.0, rgba(0, 0, 0, 120));
    p.rrect(r, 8.0, rgb(30, 27, 24));
    p.rrect_stroke(r, 8.0, 1.0, rgb(80, 70, 56));
    app.hits.push((r, Hit::Block));
    let mut y = r.y + 4.0;
    if menu.scroll == 0 {
        p.text_in(Font::Bold, 8.0, "FACTORY", Rect::new(r.x + 12.0, y, r.w, row), 0.0, 0.5, GOLD);
        y += row;
    }
    for (i, name) in shown {
        if i == user_from {
            p.fill(Rect::new(r.x + 8.0, y + 2.0, r.w - 16.0, 1.0), rgb(70, 62, 52));
            p.text_in(Font::Bold, 8.0, "USER", Rect::new(r.x + 12.0, y, r.w, row), 0.0, 0.5, GOLD);
            y += row;
        }
        let ir = Rect::new(r.x + 4.0, y, r.w - 8.0, row);
        if hot(app, Hit::MenuItem(i)) {
            p.rrect(ir, 4.0, rgb(70, 58, 36));
        }
        if name == app.preset_name {
            p.text_in(Font::Bold, 9.5, "✓", Rect::new(ir.x + 6.0, y, 20.0, row), 0.0, 0.5, GOLD);
        }
        p.text_in(Font::Regular, 9.5, &name, Rect::new(ir.x + 28.0, y, ir.w - 30.0, row), 0.0, 0.5, TEXT);
        app.hits.push((ir, Hit::MenuItem(i)));
        y += row;
    }
    if is_user {
        p.fill(Rect::new(r.x + 8.0, y + 3.0, r.w - 16.0, 1.0), rgb(70, 62, 52));
        let dr = Rect::new(r.x + 4.0, y + 6.0, r.w - 8.0, row);
        if hot(app, Hit::MenuDelete) {
            p.rrect(dr, 4.0, rgb(70, 40, 36));
        }
        let label = format!("Delete \"{}\"", app.preset_name);
        p.text_in(Font::Regular, 9.5, &label, Rect::new(dr.x + 28.0, dr.y, dr.w - 30.0, row), 0.0, 0.5, rgb(240, 140, 120));
        app.hits.push((dr, Hit::MenuDelete));
    }
}

fn dialog(app: &mut App, p: &mut Painter, w: f32, h: f32) {
    let Some(d) = &app.dialog else { return };
    let text = d.text.clone();
    p.fill(Rect::new(0.0, 0.0, w, h), rgba(0, 0, 0, 140));
    app.hits.push((Rect::new(0.0, 0.0, w, h), Hit::Block));
    let r = Rect::centered(w / 2.0, h / 2.0, 420.0, 170.0);
    p.rrect(r, 10.0, rgb(24, 21, 19));
    p.rrect_stroke(r, 10.0, 1.0, with_alpha(GOLD, 140));
    p.text(Font::Bold, 12.0, "Save Preset", r.x + 22.0, r.y + 18.0, GOLD);
    let field = Rect::new(r.x + 22.0, r.y + 56.0, r.w - 44.0, 34.0);
    p.rrect(field, 5.0, rgb(40, 36, 32));
    p.rrect_stroke(field, 5.0, 1.0, rgb(90, 78, 60));
    let caret = if std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() % 1000 < 500).unwrap_or(true) { "|" } else { "" };
    p.text_in(Font::Regular, 10.5, &format!("{text}{caret}"), Rect::new(field.x + 10.0, field.y, field.w - 20.0, field.h), 0.0, 0.5, TEXT);
    button(app, p, Rect::new(r.right() - 22.0 - 110.0, r.bottom() - 52.0, 110.0, 32.0), "SAVE", Hit::DialogOk, true, 9.0);
    button(app, p, Rect::new(r.right() - 22.0 - 230.0, r.bottom() - 52.0, 110.0, 32.0), "CANCEL", Hit::DialogCancel, false, 9.0);
}

// ===================================================================== audio settings (standalone)

/// A "◀ value ▶" selector row.
fn selector(app: &mut App, p: &mut Painter, label: &str, value: &str, y: f32, r: Rect, f: Field) {
    p.text_in(Font::Bold, 8.0, label, Rect::new(r.x + 24.0, y, 150.0, 32.0), 0.0, 0.5, DIM);
    let x0 = r.x + 170.0;
    let w = r.right() - 24.0 - x0;
    button(app, p, Rect::new(x0, y, 32.0, 32.0), "◀", Hit::Cycle(f, -1), false, 8.5);
    let vr = Rect::new(x0 + 38.0, y, w - 76.0, 32.0);
    p.rrect(vr, 5.0, rgb(30, 27, 24));
    p.rrect_stroke(vr, 5.0, 1.0, rgb(80, 72, 60));
    p.text_fit(Font::Semibold, 9.5, value, vr.inflate(-10.0, 0.0), 0.5, TEXT);
    button(app, p, Rect::new(x0 + w - 32.0, y, 32.0, 32.0), "▶", Hit::Cycle(f, 1), false, 8.5);
}

fn audio_settings(app: &mut App, p: &mut Painter, w: f32, h: f32) {
    use crate::standalone::Driver;
    let Some(a) = &app.audio else { return };
    let Some(pend) = a.pending.clone() else { return };
    let (devices, asio_inputs, running, status) = (a.devices.clone(), a.asio_inputs.clone(), a.host.running(), a.error.clone().unwrap_or_else(|| a.host.status()));

    p.fill(Rect::new(0.0, 0.0, w, h), rgba(0, 0, 0, 150));
    app.hits.push((Rect::new(0.0, 0.0, w, h), Hit::Block));
    let r = Rect::centered(w / 2.0, h / 2.0, 600.0, 430.0);
    p.rrect(Rect::new(r.x + 6.0, r.y + 10.0, r.w, r.h), 12.0, rgba(0, 0, 0, 120));
    p.grad(r, 12.0, rgb(36, 32, 28), rgb(20, 18, 16));
    p.rrect_stroke(r, 12.0, 1.0, with_alpha(GOLD, 140));
    p.text(Font::Bold, 14.0, "AUDIO SETTINGS", r.x + 24.0, r.y + 18.0, GOLD);

    let mut y = r.y + 64.0;
    p.text_in(Font::Bold, 8.0, "DRIVER", Rect::new(r.x + 24.0, y, 150.0, 32.0), 0.0, 0.5, DIM);
    let names = ["ASIO", "WASAPI", "DI FILE"];
    let cur = match pend.driver {
        Driver::Asio => 0,
        Driver::Wasapi => 1,
        Driver::DiFile => 2,
    };
    let bw = (r.right() - 24.0 - (r.x + 170.0) - 16.0) / 3.0;
    for (i, n) in names.iter().enumerate() {
        button(app, p, Rect::new(r.x + 170.0 + i as f32 * (bw + 8.0), y, bw, 32.0), n, Hit::SetDriver(i as u8), cur == i, 8.5);
    }
    y += 52.0;

    let hint;
    match pend.driver {
        Driver::Asio => {
            let drv = if devices.asio.is_empty() { "no ASIO drivers installed".to_string() } else { pend.asio_driver.clone() };
            selector(app, p, "ASIO DRIVER", &drv, y, r, Field::AsioDriver);
            y += 44.0;
            let ch = asio_inputs.get(pend.input_channel).cloned().unwrap_or_else(|| format!("Input {}", pend.input_channel + 1));
            selector(app, p, "INPUT CHANNEL", &ch, y, r, Field::AsioInput);
            y += 52.0;
            let can_panel = running && a_driver_is(app, Driver::Asio, &pend.asio_driver);
            let pr = Rect::new(r.x + 170.0, y, 200.0, 32.0);
            button(app, p, pr, "ASIO CONTROL PANEL…", if can_panel { Hit::AsioPanel } else { Hit::Block }, false, 8.5);
            if !can_panel {
                p.rrect(pr, 5.0, rgba(20, 18, 16, 170)); // disabled until this driver is running
            }
            hint = if can_panel {
                "Buffer size and sample rate are set in the driver's control panel; the rig restarts itself when they change."
            } else {
                "Apply to start this driver; its control panel (buffer size, sample rate) opens once it is running."
            };
        }
        Driver::Wasapi => {
            let inp = if pend.input_device.is_empty() { "Default input".to_string() } else { pend.input_device.clone() };
            selector(app, p, "INPUT DEVICE", &inp, y, r, Field::WasapiIn);
            y += 44.0;
            selector(app, p, "INPUT CHANNEL", &format!("Input {}", pend.input_channel + 1), y, r, Field::WasapiChannel);
            y += 44.0;
            let out = if pend.output_device.is_empty() { "Default output".to_string() } else { pend.output_device.clone() };
            selector(app, p, "OUTPUT DEVICE", &out, y, r, Field::WasapiOut);
            hint = "Windows shared mode: input and output must run at the same sample rate. ASIO gives much lower latency if your interface has a driver.";
        }
        Driver::DiFile => {
            p.text_in(Font::Bold, 8.0, "DI RECORDING", Rect::new(r.x + 24.0, y, 150.0, 32.0), 0.0, 0.5, DIM);
            button(app, p, Rect::new(r.x + 170.0, y, 110.0, 32.0), "CHOOSE…", Hit::ChooseDi, false, 8.5);
            let name = if pend.di_file.is_empty() {
                "no file chosen".to_string()
            } else {
                std::path::Path::new(&pend.di_file).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
            };
            p.text_fit(Font::Regular, 9.0, &name, Rect::new(r.x + 290.0, y, r.right() - 24.0 - (r.x + 290.0), 32.0), 0.0, TEXT);
            y += 44.0;
            let out = if pend.output_device.is_empty() { "Default output".to_string() } else { pend.output_device.clone() };
            selector(app, p, "OUTPUT DEVICE", &out, y, r, Field::WasapiOut);
            hint = "Plays a dry DI recording (.wav) through the rig in a loop: dial in tones without a guitar.";
        }
    }

    // hint, wrapped over two lines at a word boundary
    let hr = Rect::new(r.x + 24.0, r.bottom() - 122.0, r.w - 48.0, 34.0);
    let (a1, a2) = wrap2(hint, hr.w);
    p.text(Font::Regular, 8.5, &a1, hr.x, hr.y, DIM);
    p.text(Font::Regular, 8.5, &a2, hr.x, hr.y + 16.0, DIM);
    let col = if running { rgb(120, 220, 140) } else { rgb(230, 120, 90) };
    p.text_fit(Font::Semibold, 8.5, &status, Rect::new(r.x + 24.0, r.bottom() - 82.0, r.w - 48.0, 20.0), 0.0, col);
    button(app, p, Rect::new(r.right() - 24.0 - 150.0, r.bottom() - 52.0, 150.0, 34.0), "APPLY & START", Hit::ApplyAudio, true, 9.0);
    button(app, p, Rect::new(r.right() - 24.0 - 270.0, r.bottom() - 52.0, 110.0, 34.0), "CANCEL", Hit::CloseSettings, false, 9.0);
}

fn a_driver_is(app: &App, d: crate::standalone::Driver, asio: &str) -> bool {
    app.audio.as_ref().is_some_and(|a| a.settings.driver == d && a.settings.asio_driver == asio)
}

fn wrap2(s: &str, width: f32) -> (String, String) {
    let mut first = String::new();
    let mut words = s.split(' ').peekable();
    while let Some(w) = words.peek() {
        let trial = if first.is_empty() { w.to_string() } else { format!("{first} {w}") };
        if text::measure(Font::Regular, 8.5, &trial) > width {
            break;
        }
        first = trial;
        words.next();
    }
    (first, words.collect::<Vec<_>>().join(" "))
}
