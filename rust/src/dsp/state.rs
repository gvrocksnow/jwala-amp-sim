//! A plain snapshot of every control, taken once per audio block. Keeps the DSP independent
//! of the plugin framework (the offline renderer fills it from a preset instead).

use crate::spec::{self, MAX_FX_PARAMS};
use std::collections::HashMap;

#[derive(Clone, Copy, Default)]
pub struct AmpState {
    pub gain: f32,
    pub bass: f32,
    pub middle: f32,
    pub treble: f32,
    pub presence: f32,
    pub depth: f32,
    pub master: f32,
    /// 0 clean, 1 crunch, 2 lead, 3 acoustic.
    pub channel: u8,
    pub bright: bool,
    /// Acoustic-channel switches (see `dsp::acoustic`).
    pub decay: bool,
    pub attack: bool,
    pub dynamic: bool,
}

#[derive(Clone, Copy, Default)]
pub struct CabState {
    pub enabled: bool,
    /// 0..2 built-in, 3 = user IR.
    pub model: u8,
    pub mic_pos: f32,
    pub distance: f32,
    pub low_cut: f32,
    pub high_cut: f32,
}

#[derive(Clone, Copy, Default)]
pub struct FxState {
    pub on: bool,
    pub p: [f32; MAX_FX_PARAMS],
}

/// The graphic EQ: band sliders, then LEVEL (see `spec::EQ_KNOBS`).
#[derive(Clone, Copy, Default)]
pub struct EqState {
    pub on: bool,
    pub p: [f32; spec::EQ_BANDS + 1],
}

#[derive(Clone, Copy, Default)]
pub struct RigState {
    pub input: f32,
    pub gate: f32,
    pub output: f32,
    pub amp: AmpState,
    pub cab: CabState,
    pub eq: EqState,
    pub pre: [FxState; 4],
    pub post: [FxState; 5],
    pub tuner_mute: bool,
}

impl RigState {
    /// Builds a state from a preset-style key map; anything missing uses its default
    /// (effects default to off), like `RigModel.Load` in the C# version.
    pub fn from_map(d: &HashMap<String, f32>) -> Self {
        let get = |k: &str, def: f32| d.get(k).copied().unwrap_or(def);
        let mut s = RigState {
            input: get("global.INPUT", spec::GLOBALS[0].default),
            gate: get("global.GATE", spec::GLOBALS[1].default),
            output: get("global.OUTPUT", spec::GLOBALS[2].default),
            ..Default::default()
        };
        let a: Vec<f32> = spec::AMP_KNOBS.iter().map(|p| get(&format!("amp.{}", p.name), p.default)).collect();
        s.amp = AmpState {
            gain: a[0],
            bass: a[1],
            middle: a[2],
            treble: a[3],
            presence: a[4],
            depth: a[5],
            master: a[6],
            channel: get("amp.CHANNEL", 1.0) as u8,
            bright: get("amp.BRIGHT", 0.0) > 0.5,
            decay: get("amp.DECAY", 0.0) > 0.5,
            attack: get("amp.ATTACK", 0.0) > 0.5,
            dynamic: get("amp.DYNAMIC", 0.0) > 0.5,
        };
        let c: Vec<f32> = spec::CAB_KNOBS.iter().map(|p| get(&format!("cab.{}", p.name), p.default)).collect();
        s.cab = CabState {
            enabled: get("cab.ON", 1.0) > 0.5,
            model: get("cab.MODEL", 0.0) as u8,
            mic_pos: c[0],
            distance: c[1],
            low_cut: c[2],
            high_cut: c[3],
        };
        s.eq.on = get("eq.ON", 0.0) > 0.5;
        for (i, ps) in spec::EQ_KNOBS.iter().enumerate() {
            s.eq.p[i] = get(&format!("eq.{}", ps.name), ps.default);
        }
        for (fx, sp) in s.pre.iter_mut().zip(spec::PRE.iter()).chain(s.post.iter_mut().zip(spec::POST.iter())) {
            fx.on = get(&format!("{}.ON", sp.id), 0.0) > 0.5;
            for (i, ps) in sp.params.iter().enumerate() {
                fx.p[i] = get(&format!("{}.{}", sp.id, ps.name), ps.default);
            }
        }
        s
    }
}
