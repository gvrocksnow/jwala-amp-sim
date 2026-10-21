//! Host-visible parameters. IDs are the preset keys ("amp.GAIN", "comp.SUSTAIN", "delay.ON"…),
//! so presets and plugin state share one vocabulary.

use crate::dsp::state::{AmpState, CabState, EqState, FxState, RigState};
use crate::spec::{self, EffectSpec, ParamSpec};
use nih_plug::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

/// One effect's bypass switch and knobs.
pub struct FxParams {
    pub spec: &'static EffectSpec,
    pub on: BoolParam,
    pub knobs: Vec<FloatParam>,
}

/// Non-parameter state saved with the plugin (and restored by the host).
#[derive(Default, Clone)]
pub struct Persisted {
    pub preset: String,
    pub ir_path: String,
    pub editor_size: (u32, u32),
}

pub struct TubeParams {
    pub globals: Vec<FloatParam>,
    pub amp: Vec<FloatParam>,
    pub channel: IntParam,
    pub bright: BoolParam,
    pub decay: BoolParam,
    pub attack: BoolParam,
    pub dynamic: BoolParam,
    pub cab_on: BoolParam,
    pub cab_model: IntParam,
    pub cab: Vec<FloatParam>,
    pub eq_on: BoolParam,
    /// Graphic EQ bands, then LEVEL.
    pub eq: Vec<FloatParam>,
    pub pre: Vec<FxParams>,
    pub post: Vec<FxParams>,
    pub persisted: RwLock<Persisted>,
}

/// Default editor size in logical pixels (the C# window's client area).
pub const EDITOR_SIZE: (u32, u32) = (1320, 820);
pub const EDITOR_MIN: (u32, u32) = (1180, 720);

fn title(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn knob(prefix: &str, ps: &ParamSpec) -> FloatParam {
    let fmt = ps.fmt;
    FloatParam::new(format!("{prefix} {}", title(ps.name)), ps.default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_value_to_string(Arc::new(move |v| fmt(v)))
}

fn fx(spec: &'static EffectSpec) -> FxParams {
    let pretty = title(spec.name);
    FxParams {
        spec,
        on: BoolParam::new(format!("{pretty} On"), false),
        knobs: spec.params.iter().map(|p| knob(&pretty, p)).collect(),
    }
}

impl Default for TubeParams {
    fn default() -> Self {
        TubeParams {
            globals: spec::GLOBALS.iter().map(|p| knob("Global", p)).collect(),
            amp: spec::AMP_KNOBS.iter().map(|p| knob("Amp", p)).collect(),
            channel: IntParam::new("Amp Channel", 1, IntRange::Linear { min: 0, max: 3 })
                .with_value_to_string(Arc::new(|v| spec::CHANNELS[v.clamp(0, 3) as usize].to_string())),
            bright: BoolParam::new("Amp Bright", false),
            decay: BoolParam::new("Amp Decay", false),
            attack: BoolParam::new("Amp Attack", false),
            dynamic: BoolParam::new("Amp Dynamic", false),
            cab_on: BoolParam::new("Cab On", true),
            cab_model: IntParam::new("Cab Model", 0, IntRange::Linear { min: 0, max: 3 })
                .with_value_to_string(Arc::new(|v| spec::CAB_MODELS[v.clamp(0, 3) as usize].to_string())),
            cab: spec::CAB_KNOBS.iter().map(|p| knob("Cab", p)).collect(),
            eq_on: BoolParam::new("EQ On", false),
            eq: spec::EQ_KNOBS.iter().map(|p| knob("EQ", p)).collect(),
            pre: spec::PRE.iter().map(fx).collect(),
            post: spec::POST.iter().map(fx).collect(),
            persisted: RwLock::new(Persisted { editor_size: EDITOR_SIZE, ..Default::default() }),
        }
    }
}

/// A reference to any normalised knob, used by the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Knob {
    Global(usize),
    Amp(usize),
    Cab(usize),
    Eq(usize),
    Pre(usize, usize),
    Post(usize, usize),
}

impl TubeParams {
    pub fn knob(&self, k: Knob) -> &FloatParam {
        match k {
            Knob::Global(i) => &self.globals[i],
            Knob::Amp(i) => &self.amp[i],
            Knob::Cab(i) => &self.cab[i],
            Knob::Eq(i) => &self.eq[i],
            Knob::Pre(f, i) => &self.pre[f].knobs[i],
            Knob::Post(f, i) => &self.post[f].knobs[i],
        }
    }

    pub fn knob_spec(&self, k: Knob) -> &'static ParamSpec {
        match k {
            Knob::Global(i) => &spec::GLOBALS[i],
            Knob::Amp(i) => &spec::AMP_KNOBS[i],
            Knob::Cab(i) => &spec::CAB_KNOBS[i],
            Knob::Eq(i) => &spec::EQ_KNOBS[i],
            Knob::Pre(f, i) => &spec::PRE[f].params[i],
            Knob::Post(f, i) => &spec::POST[f].params[i],
        }
    }

    /// Snapshot for the audio thread (no allocation).
    pub fn snapshot(&self) -> RigState {
        let a = |i: usize| self.amp[i].value();
        let fxs = |f: &FxParams| {
            let mut s = FxState { on: f.on.value(), ..Default::default() };
            for (i, k) in f.knobs.iter().enumerate() {
                s.p[i] = k.value();
            }
            s
        };
        let mut st = RigState {
            input: self.globals[0].value(),
            gate: self.globals[1].value(),
            output: self.globals[2].value(),
            amp: AmpState {
                gain: a(0),
                bass: a(1),
                middle: a(2),
                treble: a(3),
                presence: a(4),
                depth: a(5),
                master: a(6),
                channel: self.channel.value() as u8,
                bright: self.bright.value(),
                decay: self.decay.value(),
                attack: self.attack.value(),
                dynamic: self.dynamic.value(),
            },
            cab: CabState {
                enabled: self.cab_on.value(),
                model: self.cab_model.value() as u8,
                mic_pos: self.cab[0].value(),
                distance: self.cab[1].value(),
                low_cut: self.cab[2].value(),
                high_cut: self.cab[3].value(),
            },
            eq: EqState { on: self.eq_on.value(), p: std::array::from_fn(|i| self.eq[i].value()) },
            ..Default::default()
        };
        for (s, f) in st.pre.iter_mut().zip(&self.pre) {
            *s = fxs(f);
        }
        for (s, f) in st.post.iter_mut().zip(&self.post) {
            *s = fxs(f);
        }
        st
    }

    /// The current settings as a preset-style key map (what user presets store).
    pub fn to_map(&self) -> HashMap<String, f32> {
        let mut d = HashMap::new();
        for (p, s) in self.globals.iter().zip(&spec::GLOBALS) {
            d.insert(format!("global.{}", s.name), p.value());
        }
        for (p, s) in self.amp.iter().zip(&spec::AMP_KNOBS) {
            d.insert(format!("amp.{}", s.name), p.value());
        }
        d.insert("amp.CHANNEL".into(), self.channel.value() as f32);
        d.insert("amp.BRIGHT".into(), self.bright.value() as i32 as f32);
        d.insert("amp.DECAY".into(), self.decay.value() as i32 as f32);
        d.insert("amp.ATTACK".into(), self.attack.value() as i32 as f32);
        d.insert("amp.DYNAMIC".into(), self.dynamic.value() as i32 as f32);
        d.insert("cab.ON".into(), self.cab_on.value() as i32 as f32);
        d.insert("cab.MODEL".into(), self.cab_model.value() as f32);
        for (p, s) in self.cab.iter().zip(&spec::CAB_KNOBS) {
            d.insert(format!("cab.{}", s.name), p.value());
        }
        d.insert("eq.ON".into(), self.eq_on.value() as i32 as f32);
        for (p, s) in self.eq.iter().zip(&spec::EQ_KNOBS) {
            d.insert(format!("eq.{}", s.name), p.value());
        }
        for f in self.pre.iter().chain(&self.post) {
            d.insert(format!("{}.ON", f.spec.id), f.on.value() as i32 as f32);
            for (p, s) in f.knobs.iter().zip(f.spec.params) {
                d.insert(format!("{}.{}", f.spec.id, s.name), p.value());
            }
        }
        d
    }

    /// Every parameter with its id, in a stable order.
    fn all(&self) -> Vec<(String, ParamPtr, String)> {
        let mut v = Vec::new();
        for (p, s) in self.globals.iter().zip(&spec::GLOBALS) {
            v.push((format!("global.{}", s.name), p.as_ptr(), "Global".into()));
        }
        for (p, s) in self.amp.iter().zip(&spec::AMP_KNOBS) {
            v.push((format!("amp.{}", s.name), p.as_ptr(), "Amp".into()));
        }
        v.push(("amp.CHANNEL".into(), self.channel.as_ptr(), "Amp".into()));
        v.push(("amp.BRIGHT".into(), self.bright.as_ptr(), "Amp".into()));
        v.push(("amp.DECAY".into(), self.decay.as_ptr(), "Amp".into()));
        v.push(("amp.ATTACK".into(), self.attack.as_ptr(), "Amp".into()));
        v.push(("amp.DYNAMIC".into(), self.dynamic.as_ptr(), "Amp".into()));
        v.push(("cab.ON".into(), self.cab_on.as_ptr(), "Cab".into()));
        v.push(("cab.MODEL".into(), self.cab_model.as_ptr(), "Cab".into()));
        for (p, s) in self.cab.iter().zip(&spec::CAB_KNOBS) {
            v.push((format!("cab.{}", s.name), p.as_ptr(), "Cab".into()));
        }
        v.push(("eq.ON".into(), self.eq_on.as_ptr(), "EQ".into()));
        for (p, s) in self.eq.iter().zip(&spec::EQ_KNOBS) {
            v.push((format!("eq.{}", s.name), p.as_ptr(), "EQ".into()));
        }
        for f in self.pre.iter().chain(&self.post) {
            let group = title(f.spec.name);
            v.push((format!("{}.ON", f.spec.id), f.on.as_ptr(), group.clone()));
            for (p, s) in f.knobs.iter().zip(f.spec.params) {
                v.push((format!("{}.{}", f.spec.id, s.name), p.as_ptr(), group.clone()));
            }
        }
        v
    }

    /// Looks up a parameter by preset key.
    pub fn ptr(&self, key: &str) -> Option<ParamPtr> {
        self.all().into_iter().find(|(k, _, _)| k == key).map(|(_, p, _)| p)
    }
}

unsafe impl Params for TubeParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        // ids must be at most 32 bytes for the CLAP/VST3 wrappers; ours are well under that
        self.all()
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        let p = self.persisted.read().unwrap();
        let mut m = BTreeMap::new();
        m.insert("preset".into(), p.preset.clone());
        m.insert("ir_path".into(), p.ir_path.clone());
        m.insert("editor_size".into(), format!("{}x{}", p.editor_size.0, p.editor_size.1));
        m
    }

    fn deserialize_fields(&self, serialized: &BTreeMap<String, String>) {
        let mut p = self.persisted.write().unwrap();
        if let Some(v) = serialized.get("preset") {
            p.preset = v.clone();
        }
        if let Some(v) = serialized.get("ir_path") {
            p.ir_path = v.clone();
        }
        if let Some((w, h)) = serialized.get("editor_size").and_then(|v| v.split_once('x')) {
            if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                p.editor_size = (u32::max(w, EDITOR_MIN.0), u32::max(h, EDITOR_MIN.1));
            }
        }
    }
}
