//! The running signal chain at one sample rate: gate → stomps → amp → cab → graphic EQ → stereo FX.

use super::amp::AmpEngine;
use super::convolver::{Convolver, PreparedIr};
use super::delay_line::DelayLine;
use super::eq::GraphicEq;
use super::filters::{Biquad, OnePole};
use super::pedals::{make_post, make_pre, StereoFx, Stomp};
use super::state::RigState;
use crate::spec::{self, Slot};

/// Early reflections off the room (delay in seconds, gain), slightly different per side.
const ROOM_L: [(f64, f64); 5] = [(0.0029, 0.55), (0.0047, -0.42), (0.0073, 0.33), (0.0109, -0.24), (0.0157, 0.17)];
const ROOM_R: [(f64, f64); 5] = [(0.0034, 0.52), (0.0055, -0.40), (0.0082, 0.31), (0.0121, -0.22), (0.0171, 0.15)];

/// Schroeder allpass: smears the room's reflections so they don't ring.
struct Allpass {
    buf: Vec<f64>,
    i: usize,
    g: f64,
}

impl Allpass {
    fn new(len: f64, g: f64) -> Self {
        Allpass { buf: vec![0.0; (len as usize).max(1)], i: 0, g }
    }
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let d = self.buf[self.i];
        let w = x + self.g * d;
        self.buf[self.i] = w;
        self.i = (self.i + 1) % self.buf.len();
        d - self.g * w
    }
}

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Transparent below 0.8, smooth limiting above, never exceeds ±1.
fn clip(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    let ax = x.abs();
    if ax <= 0.8 {
        return x;
    }
    x.signum() * (0.8 + 0.2 * ((ax - 0.8) / 0.2).tanh())
}

/// Meter readings for the UI.
#[derive(Default, Clone, Copy)]
pub struct Meters {
    pub input: f32,
    pub out_l: f32,
    pub out_r: f32,
}

pub struct Rig {
    pub sample_rate: f64,
    pub meters: Meters,
    amp: AmpEngine,
    /// Base-rate pedals (compressor), with their index in `spec::PRE`.
    mono: Vec<(usize, Stomp)>,
    /// Pedals inside the amp's oversampled region, with their index in `spec::PRE`.
    os: Vec<(usize, Stomp)>,
    os_on: Vec<bool>,
    post: Vec<Box<dyn StereoFx>>,
    eq: GraphicEq,
    cab: Convolver,
    mic_shelf: [Biquad; 2],
    mic_lp: [Biquad; 2],
    proximity: [Biquad; 2],
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    room: DelayLine,
    room_lp: [OnePole; 2],
    room_ap: [[Allpass; 2]; 2],
    last_mic: [f32; 4],
    in_hp: OnePole,
    out_dc_l: OnePole,
    out_dc_r: OnePole,
    gate_env: f64,
    gate_gain: f64,
    gate_det: f64,
    gate_atk: f64,
    gate_rel: f64,
    s_in: f64,
    s_out: f64,
    smooth: f64,
    tmp: Vec<f32>,
}

impl Rig {
    /// Allocates everything; call off the audio thread. `max_block` sizes the scratch buffer.
    pub fn new(fs: f64, st: &RigState, max_block: usize) -> Self {
        let amp = AmpEngine::new(fs, max_block.max(1));
        let mut mono = Vec::new();
        let mut os = Vec::new();
        for (i, sp) in spec::PRE.iter().enumerate() {
            let mut s = Stomp::new(make_pre(sp.id));
            if sp.slot == Slot::Mono {
                s.prepare(fs, st.pre[i].on);
                mono.push((i, s));
            } else {
                s.prepare(amp.os_rate(), st.pre[i].on);
                os.push((i, s));
            }
        }
        let post = spec::POST
            .iter()
            .enumerate()
            .map(|(i, sp)| {
                let mut fx = make_post(sp.id);
                fx.prepare(fs, st.post[i].on, &st.post[i].p);
                fx
            })
            .collect();
        let mut rig = Rig {
            sample_rate: fs,
            meters: Meters::default(),
            amp,
            os_on: vec![false; os.len()],
            mono,
            os,
            post,
            eq: GraphicEq::new(fs, &st.eq),
            cab: Convolver::default(),
            mic_shelf: [Biquad::default(); 2],
            mic_lp: [Biquad::default(); 2],
            proximity: [Biquad::default(); 2],
            low_cut: [Biquad::default(); 2],
            high_cut: [Biquad::default(); 2],
            room: DelayLine::new((fs * 0.02) as usize),
            room_lp: [OnePole::default(); 2],
            room_ap: [
                [Allpass::new(fs * 0.0013, 0.5), Allpass::new(fs * 0.0023, 0.5)],
                [Allpass::new(fs * 0.0017, 0.5), Allpass::new(fs * 0.0029, 0.5)],
            ],
            last_mic: [-1.0; 4],
            in_hp: OnePole::default(),
            out_dc_l: OnePole::default(),
            out_dc_r: OnePole::default(),
            gate_env: 0.0,
            gate_gain: 0.0,
            gate_det: 1.0 - (-1.0 / (fs * 0.04)).exp(),
            gate_atk: 1.0 - (-1.0 / (fs * 0.0008)).exp(),
            gate_rel: 1.0 - (-1.0 / (fs * 0.07)).exp(),
            s_in: 0.0,
            s_out: 0.0,
            smooth: 1.0 - (-1.0 / (fs * 0.02)).exp(),
            tmp: vec![0.0; max_block.max(1)],
        };
        rig.in_hp.set_low_pass(fs, 25.0);
        rig.out_dc_l.set_low_pass(fs, 10.0);
        rig.out_dc_r.set_low_pass(fs, 10.0);
        for lp in rig.room_lp.iter_mut() {
            lp.set_low_pass(fs, 5500.0); // the walls soak up the top
        }
        rig
    }

    /// Installs a cab IR (built off the audio thread); returns the old one to drop elsewhere.
    pub fn swap_cab(&mut self, ir: PreparedIr) -> PreparedIr {
        self.cab.swap(ir)
    }

    fn update_mic(&mut self, st: &RigState) {
        let c = &st.cab;
        let fs = self.sample_rate;
        // both channels share coefficients; each keeps its own state
        fn set(f: &mut [Biquad; 2], design: impl FnOnce(&mut Biquad)) {
            design(&mut f[0]);
            let [a, b] = f;
            b.copy_coefs(a);
        }
        self.cab.set_mic(c.mic_pos);
        if c.mic_pos != self.last_mic[0] {
            self.last_mic[0] = c.mic_pos;
            // only for user IRs and the acoustic channel: the built-in cabs are designed per position
            let p = c.mic_pos as f64;
            set(&mut self.mic_shelf, |b| b.high_shelf(fs, 2500.0, 2.5 - 10.0 * p)); // cap = bright, cone = dark
            set(&mut self.mic_lp, |b| b.low_pass(fs, 16000.0 - 9000.0 * p, 0.7));
        }
        if c.distance != self.last_mic[1] {
            self.last_mic[1] = c.distance;
            let d = c.distance as f64;
            set(&mut self.proximity, |b| b.low_shelf(fs, 180.0, 3.5 - 8.0 * d)); // proximity effect fades with distance
        }
        if c.low_cut != self.last_mic[2] {
            self.last_mic[2] = c.low_cut;
            let v = c.low_cut as f64;
            set(&mut self.low_cut, |b| b.high_pass(fs, 20.0 * 15f64.powf(v), 0.707));
        }
        if c.high_cut != self.last_mic[3] {
            self.last_mic[3] = c.high_cut;
            let v = c.high_cut as f64;
            set(&mut self.high_cut, |b| b.low_pass(fs, (0.45 * fs).min(3000.0 * (20000.0f64 / 3000.0).powf(v)), 0.707));
        }
    }

    /// Mono in, stereo out. `muted` silences the output (tuner open with mute on).
    /// Blocks longer than `max_block` are processed in chunks.
    pub fn process(&mut self, input: &[f32], l: &mut [f32], r: &mut [f32], st: &RigState, muted: bool) {
        let max = self.tmp.len();
        let mut off = 0;
        while off < input.len() {
            let n = (input.len() - off).min(max);
            self.process_chunk(&input[off..off + n], &mut l[off..off + n], &mut r[off..off + n], st, muted);
            off += n;
        }
    }

    fn process_chunk(&mut self, input: &[f32], l: &mut [f32], r: &mut [f32], st: &RigState, muted: bool) {
        let n = input.len();
        let t_in = db(-12.0 + 24.0 * st.input as f64);
        let t_out = if st.output < 0.001 || muted { 0.0 } else { db(-40.0 + 46.0 * st.output as f64) };
        let gate_thr = if st.gate < 0.01 { 0.0 } else { db(-80.0 + 50.0 * st.gate as f64) };

        // input gain, DC/rumble block, noise gate, base-rate pedals (compressor)
        let mut in_pk: f32 = 0.0;
        for (i, s) in self.mono.iter_mut() {
            s.fx.update(&st.pre[*i].p);
        }
        if st.cab.enabled {
            self.update_mic(st);
        }
        let buf = &mut self.tmp[..n];
        for i in 0..n {
            self.s_in += self.smooth * (t_in - self.s_in);
            let mut x = self.in_hp.high_pass(input[i] as f64 * self.s_in);
            in_pk = in_pk.max(x.abs() as f32);
            self.gate_env += self.gate_det * (x.abs() - self.gate_env);
            let gt = if gate_thr == 0.0 {
                1.0
            } else if self.gate_env > gate_thr {
                1.0
            } else if self.gate_env < gate_thr * 0.5 {
                0.0
            } else if self.gate_gain > 0.5 {
                1.0
            } else {
                0.0
            };
            self.gate_gain += (if gt > self.gate_gain { self.gate_atk } else { self.gate_rel }) * (gt - self.gate_gain);
            x *= self.gate_gain;
            for (j, s) in self.mono.iter_mut() {
                x = s.run(x, st.pre[*j].on);
            }
            buf[i] = x as f32;
        }

        for ((i, s), on) in self.os.iter_mut().zip(self.os_on.iter_mut()) {
            s.fx.update(&st.pre[*i].p);
            *on = st.pre[*i].on;
        }
        self.amp.process(buf, &st.amp, &mut self.os, &self.os_on);

        // an acoustic guitar isn't played through a speaker: keep only the mic and room
        let acoustic = st.amp.channel == 3;
        if st.cab.enabled && !acoustic {
            self.cab.process(buf, l, r);
        } else {
            l.copy_from_slice(buf);
            r.copy_from_slice(buf);
        }
        if st.cab.enabled {
            let fs = self.sample_rate;
            let mic_eq = acoustic || !self.cab.positional();
            let room_mix = st.cab.distance as f64 * 0.3;
            for i in 0..n {
                let (xl, xr) = (l[i] as f64, r[i] as f64);
                self.room.write(0.5 * (xl + xr));
                let er = |taps: &[(f64, f64)]| taps.iter().map(|&(t, g)| g * self.room.read(fs * t)).sum::<f64>();
                let (el, erl) = (er(&ROOM_L), er(&ROOM_R));
                for (c, (x, e)) in [(xl, el), (xr, erl)].into_iter().enumerate() {
                    let [a0, a1] = &mut self.room_ap[c];
                    let e = a1.process(a0.process(self.room_lp[c].low_pass(e)));
                    let mut y = self.proximity[c].process(x + room_mix * e);
                    if mic_eq {
                        y = self.mic_lp[c].process(self.mic_shelf[c].process(y));
                    }
                    let y = self.high_cut[c].process(self.low_cut[c].process(y)) as f32;
                    if c == 0 {
                        l[i] = y;
                    } else {
                        r[i] = y;
                    }
                }
            }
        }

        if st.amp.channel == 3 {
            // the acoustic channel is a stereo pair of mics; its side skips the cab's mic and room
            for ((l, r), s) in l.iter_mut().zip(r.iter_mut()).zip(self.amp.acoustic_side(n)) {
                *l += s;
                *r -= s;
            }
        }
        self.eq.process(l, r, &st.eq);
        for (fx, s) in self.post.iter_mut().zip(st.post.iter()) {
            fx.process(l, r, &s.p, s.on);
        }

        let (mut pl, mut pr): (f32, f32) = (0.0, 0.0);
        for i in 0..n {
            self.s_out += self.smooth * (t_out - self.s_out);
            l[i] = clip(self.out_dc_l.high_pass(l[i] as f64) * self.s_out) as f32;
            r[i] = clip(self.out_dc_r.high_pass(r[i] as f64) * self.s_out) as f32;
            pl = pl.max(l[i].abs());
            pr = pr.max(r[i].abs());
        }
        let m = &mut self.meters;
        m.input = in_pk.max(m.input * 0.85);
        m.out_l = pl.max(m.out_l * 0.85);
        m.out_r = pr.max(m.out_r * 0.85);
    }
}

