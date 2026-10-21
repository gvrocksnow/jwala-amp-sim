//! Static description of every control: ids, names, defaults and value formatting.
//! Shared by the plugin parameters, the presets and the UI.

/// Value formatter for a normalised 0..1 knob.
pub type Fmt = fn(f32) -> String;

pub struct ParamSpec {
    pub name: &'static str,
    pub default: f32,
    pub fmt: Fmt,
}

const fn p(name: &'static str, default: f32, fmt: Fmt) -> ParamSpec {
    ParamSpec { name, default, fmt }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Runs at the base rate before the amp (compressor).
    Mono,
    /// Runs inside the amp's 4x oversampled region (drives, fuzz).
    Oversampled,
    /// Stereo, after the cab.
    Stereo,
}

pub struct EffectSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// Category caption under the pedal on the pedalboard pages.
    pub caption: &'static str,
    /// Double-width enclosure in the UI.
    pub wide: bool,
    pub has_tap: bool,
    pub slot: Slot,
    pub params: &'static [ParamSpec],
}

// ------------------------------------------------------------------ formatting helpers

pub fn log(v: f64, lo: f64, hi: f64) -> f64 {
    lo * (hi / lo).powf(v)
}
pub fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{:.0} ms", ms) }
}
pub fn fmt_hz(hz: f64) -> String {
    if hz >= 1000.0 { format!("{:.1} kHz", hz / 1000.0) } else { format!("{:.0} Hz", hz) }
}
pub fn ten(v: f32) -> String {
    format!("{:.1}", v * 10.0)
}
pub fn pct(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

pub fn delay_time_ms(v: f32) -> f64 {
    log(v as f64, 30.0, 1800.0)
}
pub fn doubler_delay_ms(v: f32) -> f64 {
    log(v as f64, 6.0, 45.0)
}
pub fn reverb_decay_s(v: f32) -> f64 {
    log(v as f64, 0.4, 25.0)
}

// ------------------------------------------------------------------ global / amp / cab

pub const GLOBALS: [ParamSpec; 3] = [
    p("INPUT", 0.5, |v| {
        let db = -12.0 + 24.0 * v;
        format!("{}{:.1} dB", if db >= 0.0 { "+" } else { "" }, db)
    }),
    p("GATE", 0.25, |v| if v < 0.01 { "OFF".into() } else { format!("{:.0} dB", -80.0 + 50.0 * v) }),
    p("OUTPUT", 0.6, |v| format!("{:.1} dB", -40.0 + 46.0 * v)),
];

pub const AMP_KNOBS: [ParamSpec; 7] = [
    p("GAIN", 0.6, ten),
    p("BASS", 0.5, ten),
    p("MIDDLE", 0.6, ten),
    p("TREBLE", 0.6, ten),
    p("PRESENCE", 0.5, ten),
    p("DEPTH", 0.4, ten),
    p("MASTER", 0.5, ten),
];

/// Faceplate labels for the amp knobs (the head is narrow at small window sizes).
pub const AMP_KNOB_NAMES_SHORT: [&str; 7] = ["GAIN", "BASS", "MID", "TREBLE", "PRESENCE", "DEPTH", "MASTER"];
/// The same knobs on the ACOUSTIC channel (see `dsp::acoustic`).
pub const ACOUSTIC_KNOB_NAMES: [&str; 7] = ["BODY", "BASS", "MID", "TREBLE", "AIR", "RESO", "LEVEL"];
/// The acoustic channel's switches (see `dsp::acoustic`), as printed under their LEDs.
pub const ACOUSTIC_SWITCHES: [&str; 3] = ["DECAY", "ATTACK", "DYNAMIC"];

pub const CHANNELS: [&str; 4] = ["CLEAN", "CRUNCH", "LEAD", "ACOUSTIC"];
/// Index of the ACOUSTIC channel.
pub const ACOUSTIC: i32 = 3;

pub const CAB_KNOBS: [ParamSpec; 4] = [
    p("MIC POS", 0.3, |v| (if v < 0.15 { "CAP" } else if v < 0.6 { "CAP EDGE" } else { "CONE" }).into()),
    p("DISTANCE", 0.15, |v| format!("{:.0} cm", 1.0 + v * 30.0)),
    p("LOW CUT", 0.15, |v| fmt_hz(20.0 * 15f64.powf(v as f64))),
    p("HIGH CUT", 0.8, |v| if v > 0.98 { "OFF".into() } else { fmt_hz(3000.0 * (20000.0f64 / 3000.0).powf(v as f64)) }),
];

pub const CAB_MODELS: [&str; 4] = ["4x12 V30", "2x12 ALNICO", "1x12 OPEN", "USER IR"];

// ------------------------------------------------------------------ graphic EQ (after the cab)

/// Number of EQ bands; `EQ_KNOBS` has one more slider, LEVEL.
pub const EQ_BANDS: usize = 9;
/// Centre frequencies, one octave apart.
pub const EQ_FREQS: [f64; EQ_BANDS] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
/// Slider travel in each direction.
pub const EQ_RANGE_DB: f64 = 12.0;

/// Slider value (0..1, centre = flat) to dB.
pub fn eq_db(v: f32) -> f64 {
    (v as f64 * 2.0 - 1.0) * EQ_RANGE_DB
}
pub fn fmt_db(v: f32) -> String {
    let d = eq_db(v);
    if d.abs() < 0.05 { "0.0 dB".into() } else { format!("{:+.1} dB", d) }
}

pub const EQ_KNOBS: [ParamSpec; EQ_BANDS + 1] = [
    p("63", 0.5, fmt_db),
    p("125", 0.5, fmt_db),
    p("250", 0.5, fmt_db),
    p("500", 0.5, fmt_db),
    p("1K", 0.5, fmt_db),
    p("2K", 0.5, fmt_db),
    p("4K", 0.5, fmt_db),
    p("8K", 0.5, fmt_db),
    p("16K", 0.5, fmt_db),
    p("LEVEL", 0.5, fmt_db),
];

// ------------------------------------------------------------------ effects

pub const PRE: [EffectSpec; 4] = [
    EffectSpec {
        id: "comp", name: "SQUEEZE", caption: "COMPRESSOR", wide: false, has_tap: false, slot: Slot::Mono,
        params: &[p("SUSTAIN", 0.5, ten), p("ATTACK", 0.35, ten), p("LEVEL", 0.5, ten), p("BLEND", 1.0, pct)],
    },
    EffectSpec {
        id: "od", name: "GREEN DRIVE", caption: "OVERDRIVE", wide: false, has_tap: false, slot: Slot::Oversampled,
        params: &[p("DRIVE", 0.35, ten), p("TONE", 0.5, ten), p("LEVEL", 0.6, ten)],
    },
    EffectSpec {
        id: "dist", name: "RODENT", caption: "DISTORTION", wide: false, has_tap: false, slot: Slot::Oversampled,
        params: &[p("DIST", 0.5, ten), p("FILTER", 0.45, ten), p("VOLUME", 0.5, ten)],
    },
    EffectSpec {
        id: "fuzz", name: "MUFFLER", caption: "FUZZ", wide: false, has_tap: false, slot: Slot::Oversampled,
        params: &[p("SUSTAIN", 0.6, ten), p("TONE", 0.5, ten), p("VOLUME", 0.5, ten)],
    },
];

pub const POST: [EffectSpec; 5] = [
    EffectSpec {
        id: "doubler", name: "DOUBLER", caption: "DOUBLER", wide: false, has_tap: false, slot: Slot::Stereo,
        params: &[
            p("AMOUNT", 0.5, pct),
            p("DELAY", 0.35, |v| format!("{:.0} ms", doubler_delay_ms(v))),
            p("WIDTH", 1.0, pct),
            p("MIX", 0.85, pct),
        ],
    },
    EffectSpec {
        id: "chorus", name: "TRI CHORUS", caption: "CHORUS", wide: false, has_tap: false, slot: Slot::Stereo,
        params: &[p("RATE", 0.3, |v| format!("{:.2} Hz", log(v as f64, 0.08, 6.0))), p("DEPTH", 0.5, ten), p("MIX", 0.5, pct)],
    },
    EffectSpec {
        id: "phaser", name: "PHASE 6", caption: "PHASER", wide: false, has_tap: false, slot: Slot::Stereo,
        params: &[
            p("RATE", 0.25, |v| format!("{:.2} Hz", log(v as f64, 0.05, 8.0))),
            p("DEPTH", 0.75, ten),
            p("RESO", 0.4, ten),
            p("MIX", 0.5, pct),
        ],
    },
    EffectSpec {
        id: "delay", name: "TAPE ECHO", caption: "DELAY", wide: true, has_tap: true, slot: Slot::Stereo,
        params: &[
            p("TIME", 0.6, |v| fmt_ms(delay_time_ms(v))),
            p("FEEDBACK", 0.38, pct),
            p("MIX", 0.3, pct),
            p("TONE", 0.55, ten),
            p("MOD", 0.3, ten),
            p("SPREAD", 0.6, pct),
            p("DUCK", 0.0, ten),
        ],
    },
    EffectSpec {
        id: "reverb", name: "CATHEDRAL", caption: "REVERB", wide: true, has_tap: false, slot: Slot::Stereo,
        params: &[
            p("DECAY", 0.45, |v| format!("{:.1} s", reverb_decay_s(v))),
            p("PREDELAY", 0.15, |v| format!("{:.0} ms", v * 200.0)),
            p("SIZE", 0.6, ten),
            p("TONE", 0.5, ten),
            p("MOD", 0.4, ten),
            p("SHIMMER", 0.0, pct),
            p("MIX", 0.3, pct),
        ],
    },
];

/// Most knobs any effect has.
pub const MAX_FX_PARAMS: usize = 7;
