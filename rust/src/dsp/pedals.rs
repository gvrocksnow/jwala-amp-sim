//! Stomp boxes before the amp (compressor, drives, fuzz) and stereo effects after the cab.
//! Knob values arrive as normalised 0..1 slices in the order of `spec::PRE` / `spec::POST`.

use super::delay_line::DelayLine;
use super::filters::{Biquad, OnePole};
use crate::spec::{delay_time_ms, doubler_delay_ms, log, reverb_decay_s};
use std::f64::consts::PI;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}
fn coef(fs: f64, seconds: f64) -> f64 {
    1.0 - (-1.0 / (fs * seconds)).exp()
}

// =====================================================================  PRE (before the amp)

/// Effect running one sample at a time, at either the base rate or the 4x oversampled rate.
pub trait SampleFx: Send {
    fn init(&mut self, fs: f64);
    /// Called once per block so parameter-dependent coefficients are cheap.
    fn update(&mut self, p: &[f32]);
    fn tick(&mut self, x: f64) -> f64;
}

/// A sample effect with click-free bypass: crossfades between dry and processed.
pub struct Stomp {
    pub fx: Box<dyn SampleFx>,
    mix: f64,
    mix_step: f64,
}

impl Stomp {
    pub fn new(fx: Box<dyn SampleFx>) -> Self {
        Stomp { fx, mix: 0.0, mix_step: 0.0 }
    }
    pub fn prepare(&mut self, fs: f64, enabled: bool) {
        self.mix_step = 1.0 / (fs * 0.015);
        self.fx.init(fs);
        self.mix = if enabled { 1.0 } else { 0.0 };
    }
    #[inline]
    pub fn run(&mut self, x: f64, enabled: bool) -> f64 {
        let target = if enabled { 1.0 } else { 0.0 };
        if self.mix == 0.0 && target == 0.0 {
            return x;
        }
        self.mix = if self.mix < target { (self.mix + self.mix_step).min(target) } else { (self.mix - self.mix_step).max(target) };
        let y = self.fx.tick(x);
        x + (y - x) * self.mix
    }
}

pub fn make_pre(id: &str) -> Box<dyn SampleFx> {
    match id {
        "comp" => Box::<Compressor>::default(),
        "od" => Box::<Overdrive>::default(),
        "dist" => Box::<Distortion>::default(),
        _ => Box::<Fuzz>::default(),
    }
}

/// Studio-style optical-ish compressor with parallel blend.
#[derive(Default)]
pub struct Compressor {
    env: f64,
    gain: f64,
    atk: f64,
    rel: f64,
    fs: f64,
    thr: f64,
    slope: f64,
    makeup: f64,
    blend: f64,
}

impl SampleFx for Compressor {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.env = 0.0;
        self.gain = 1.0;
        self.rel = coef(fs, 0.25);
    }
    fn update(&mut self, p: &[f32]) {
        let s = p[0] as f64;
        self.atk = coef(self.fs, log(p[1] as f64, 0.0008, 0.05));
        self.thr = -14.0 - 34.0 * s;
        let ratio = 2.0 + 10.0 * s;
        self.slope = 1.0 - 1.0 / ratio;
        self.makeup = db(-self.thr * self.slope * 0.55 + (-12.0 + 24.0 * p[2] as f64));
        self.blend = p[3] as f64;
    }
    fn tick(&mut self, x: f64) -> f64 {
        let a = x.abs();
        self.env += (if a > self.env { self.atk } else { self.rel }) * (a - self.env);
        let lvl = 20.0 * (self.env + 1e-9).log10();
        let over = lvl - self.thr;
        // soft knee (6 dB)
        let gr = if over <= -3.0 {
            0.0
        } else if over >= 3.0 {
            over * self.slope
        } else {
            self.slope * (over + 3.0) * (over + 3.0) / 12.0
        };
        let tg = db(-gr);
        self.gain += 0.02 * (tg - self.gain);
        let y = x * self.gain * self.makeup;
        x * (1.0 - self.blend) + y * self.blend
    }
}

/// Green screamer: mid-hump soft clipping overdrive.
#[derive(Default)]
pub struct Overdrive {
    hp: OnePole,
    clip_lp: OnePole,
    tone: OnePole,
    fs: f64,
    g: f64,
    lvl: f64,
}

impl SampleFx for Overdrive {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.hp.set_low_pass(fs, 720.0);
        self.clip_lp.set_low_pass(fs, 7500.0);
        self.hp.reset();
        self.clip_lp.reset();
        self.tone.reset();
    }
    fn update(&mut self, p: &[f32]) {
        self.g = (51e3 + p[0] as f64 * 500e3) / 4.7e3;
        self.tone.set_low_pass(self.fs, log(p[1] as f64, 650.0, 7000.0));
        self.lvl = db(-18.0 + 26.0 * p[2] as f64);
    }
    fn tick(&mut self, x: f64) -> f64 {
        let hp = self.hp.high_pass(x);
        let mut c = self.clip_lp.low_pass(hp * self.g);
        c = if c > 0.0 { 0.62 * (c / 0.62).tanh() } else { 0.55 * (c / 0.55).tanh() }; // mismatched diodes
        self.tone.low_pass(x + c) * self.lvl
    }
}

/// Op-amp hard clipper (rodent style) with sweepable filter.
#[derive(Default)]
pub struct Distortion {
    hp: OnePole,
    gbw: OnePole,
    filt: OnePole,
    filt2: OnePole,
    fs: f64,
    g: f64,
    lvl: f64,
}

impl SampleFx for Distortion {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.hp.set_low_pass(fs, 70.0);
        self.hp.reset();
        self.gbw.reset();
        self.filt.reset();
        self.filt2.reset();
    }
    fn update(&mut self, p: &[f32]) {
        self.g = log(p[0] as f64, 2.0, 2200.0);
        self.gbw.set_low_pass(self.fs, (4e6 / self.g).clamp(2500.0, 20000.0)); // op-amp runs out of bandwidth at high gain
        let fc = log(1.0 - p[1] as f64, 500.0, 16000.0);
        self.filt.set_low_pass(self.fs, fc);
        self.filt2.set_low_pass(self.fs, fc * 1.6);
        self.lvl = db(-26.0 + 26.0 * p[2] as f64);
    }
    fn tick(&mut self, x: f64) -> f64 {
        let hp = self.hp.high_pass(x);
        let mut v = self.gbw.low_pass(hp * self.g);
        v = 0.65 * v / (1.0 + (v / 0.65).abs().powf(2.5)).powf(0.4); // silicon diodes to ground
        let f1 = self.filt.low_pass(v);
        self.filt2.low_pass(f1) * self.lvl
    }
}

/// Big wall-of-fuzz: two cascaded clipping stages and a mid-scoop tone control.
#[derive(Default)]
pub struct Fuzz {
    hp1: OnePole,
    hp2: OnePole,
    lp1: OnePole,
    lp2: OnePole,
    tone_lp: OnePole,
    tone_hp: OnePole,
    g1: f64,
    lvl: f64,
    tone: f64,
}

impl Fuzz {
    fn clip(v: f64) -> f64 {
        v / (1.0 + v.abs().powf(2.5)).powf(0.4)
    }
}

impl SampleFx for Fuzz {
    fn init(&mut self, fs: f64) {
        self.hp1.set_low_pass(fs, 90.0);
        self.hp2.set_low_pass(fs, 120.0);
        self.lp1.set_low_pass(fs, 6000.0);
        self.lp2.set_low_pass(fs, 5000.0);
        self.tone_lp.set_low_pass(fs, 900.0);
        self.tone_hp.set_low_pass(fs, 1100.0);
    }
    fn update(&mut self, p: &[f32]) {
        self.g1 = log(p[0] as f64, 6.0, 400.0);
        self.tone = p[1] as f64;
        self.lvl = db(-26.0 + 24.0 * p[2] as f64);
    }
    fn tick(&mut self, x: f64) -> f64 {
        let h1 = self.hp1.high_pass(x);
        let s = self.lp1.low_pass(Self::clip(h1 * self.g1));
        let h2 = self.hp2.high_pass(s);
        let s = self.lp2.low_pass(Self::clip(h2 * 18.0));
        let y = self.tone_lp.low_pass(s) * (1.0 - self.tone) + self.tone_hp.high_pass(s) * self.tone;
        y * self.lvl * 1.8
    }
}

// =====================================================================  POST (after the cab, stereo)

/// Stereo effect working on blocks. Delay/reverb keep ringing out after being switched off.
pub trait StereoFx: Send {
    /// Allocates; call off the audio thread (plugin `initialize`). `p` are the current knob values.
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]);
    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool);
}

pub fn make_post(id: &str) -> Box<dyn StereoFx> {
    match id {
        "doubler" => Box::new(Doubler::new()),
        "chorus" => Box::new(Chorus::new()),
        "phaser" => Box::new(Phaser::default()),
        "delay" => Box::new(Delay::new()),
        _ => Box::new(Reverb::new()),
    }
}

#[inline]
fn ramp(v: f64, target: f64, step: f64) -> f64 {
    if v < target { (v + step).min(target) } else { (v - step).max(target) }
}

/// Small deterministic PRNG (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One synthetic performance: smoothed random drift of timing and level.
struct Take {
    cur: f64,
    mid: f64,
    target: f64,
    gain: f64,
    gain_target: f64,
    timer: f64,
    tone: OnePole,
    rng: Rng,
}

impl Take {
    fn new(seed: u64) -> Self {
        Take { cur: 0.0, mid: 0.0, target: 0.0, gain: 1.0, gain_target: 1.0, timer: 0.0, tone: OnePole::default(), rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }
    }
    fn step(&mut self, fs: f64, slew: f64) {
        self.timer -= 1.0;
        if self.timer <= 0.0 {
            // a new "intention" every 120-380 ms, like a player drifting around the beat
            self.target = self.rng.next_f64() * 2.0 - 1.0;
            self.gain_target = 1.0 + (self.rng.next_f64() * 2.0 - 1.0) * 0.07;
            self.timer = fs * (0.12 + 0.26 * self.rng.next_f64());
        }
        // two cascaded smoothers: drift rate (= pitch) changes glide, never step
        self.mid += slew * (self.target - self.mid);
        self.cur += slew * (self.mid - self.cur);
        self.gain += slew * (self.gain_target - self.gain);
    }
}

/// Stereo doubler: fakes a double-tracked part. Two synthetic "takes" of the signal get their
/// own slowly wandering timing (and therefore pitch), level and tone, then are spread left/right.
pub struct Doubler {
    dl: DelayLine,
    a: Take,
    b: Take,
    hp_b: OnePole,
    fs: f64,
    fade: f64,
    fade_step: f64,
}

impl Doubler {
    fn new() -> Self {
        Doubler { dl: DelayLine::new(1), a: Take::new(11), b: Take::new(23), hp_b: OnePole::default(), fs: 48000.0, fade: 0.0, fade_step: 0.0 }
    }
}

impl StereoFx for Doubler {
    fn prepare(&mut self, fs: f64, enabled: bool, _p: &[f32]) {
        self.fs = fs;
        self.dl = DelayLine::new((fs * 0.06) as usize);
        self.a.tone.set_low_pass(fs, 12000.0);
        self.b.tone.set_low_pass(fs, 8500.0);
        self.hp_b.set_low_pass(fs, 90.0);
        self.fade_step = 1.0 / (fs * 0.02);
        self.fade = if enabled { 1.0 } else { 0.0 };
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        if !enabled && self.fade == 0.0 {
            return;
        }
        let fs = self.fs;
        let (amt, base_b, w, mix) = (p[0] as f64, doubler_delay_ms(p[1]) * fs / 1000.0, p[2] as f64, p[3] as f64);
        let base_a = 0.0015 * fs; // take 1 sits nearly on the original
        let (var_a, var_b) = (amt * 0.0006 * fs, amt * 0.0016 * fs); // AMOUNT 50% ~ ±5 cents of drift
        let slew = coef(fs, 0.22);
        let t = if enabled { 1.0 } else { 0.0 };
        for i in 0..l.len() {
            self.fade = ramp(self.fade, t, self.fade_step);
            let mono = 0.5 * (l[i] as f64 + r[i] as f64);
            self.dl.write(mono);
            self.a.step(fs, slew);
            self.b.step(fs, slew);
            let ra = self.dl.read(base_a + var_a * (1.0 + self.a.cur));
            let ta = self.a.tone.low_pass(ra) * self.a.gain;
            let rb = self.dl.read(base_b + var_b * (1.0 + self.b.cur));
            let tb_lp = self.b.tone.low_pass(rb);
            let tb = self.hp_b.high_pass(tb_lp) * self.b.gain;
            let mid = 0.5 * (ta + tb);
            // decorrelated takes sum ~3 dB lower
            let (wl, wr) = ((mid + w * (ta - mid)) * 1.12, (mid + w * (tb - mid)) * 1.12);
            let m = mix * self.fade;
            l[i] = (l[i] as f64 * (1.0 - m) + wl * m) as f32;
            r[i] = (r[i] as f64 * (1.0 - m) + wr * m) as f32;
        }
    }
}

/// Lush stereo tri-chorus.
pub struct Chorus {
    dl: DelayLine,
    dr: DelayLine,
    fs: f64,
    phase: f64,
    fade: f64,
    fade_step: f64,
    voice_sin: [f64; 6],
    voice_cos: [f64; 6],
}

impl Chorus {
    fn new() -> Self {
        let mut voice_sin = [0.0; 6];
        let mut voice_cos = [0.0; 6];
        for v in 0..6 {
            voice_sin[v] = (v as f64 * PI / 3.0).sin();
            voice_cos[v] = (v as f64 * PI / 3.0).cos();
        }
        Chorus { dl: DelayLine::new(1), dr: DelayLine::new(1), fs: 48000.0, phase: 0.0, fade: 0.0, fade_step: 0.0, voice_sin, voice_cos }
    }
}

impl StereoFx for Chorus {
    fn prepare(&mut self, fs: f64, enabled: bool, _p: &[f32]) {
        self.fs = fs;
        self.dl = DelayLine::new((fs * 0.03) as usize);
        self.dr = DelayLine::new((fs * 0.03) as usize);
        self.fade_step = 1.0 / (fs * 0.015);
        self.fade = if enabled { 1.0 } else { 0.0 };
        self.phase = 0.0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        if !enabled && self.fade == 0.0 {
            return;
        }
        let fs = self.fs;
        let (rate, depth, mix) = (log(p[0] as f64, 0.08, 6.0), p[1] as f64 * 0.0045 * fs, p[2] as f64);
        let (base_d, inc) = (0.0075 * fs, 2.0 * PI * rate / fs);
        let t = if enabled { 1.0 } else { 0.0 };
        for i in 0..l.len() {
            self.fade = ramp(self.fade, t, self.fade_step);
            self.phase += inc;
            if self.phase > 2.0 * PI {
                self.phase -= 2.0 * PI;
            }
            self.dl.write(l[i] as f64);
            self.dr.write(r[i] as f64);
            let (sn, cs) = (self.phase.sin(), self.phase.cos());
            let (mut wl, mut wr) = (0.0, 0.0);
            for v in 0..6 {
                let lfo = sn * self.voice_cos[v] + cs * self.voice_sin[v]; // sin(phase + v*60°)
                let d = base_d + depth * (0.5 + 0.5 * lfo);
                if v & 1 == 0 { wl += self.dl.read(d) } else { wr += self.dr.read(d) }
            }
            wl /= 3.0;
            wr /= 3.0;
            let m = mix * self.fade;
            l[i] = (l[i] as f64 * (1.0 - 0.45 * m) + wl * 0.9 * m) as f32;
            r[i] = (r[i] as f64 * (1.0 - 0.45 * m) + wr * 0.9 * m) as f32;
        }
    }
}

/// Six-stage stereo phaser with resonance.
#[derive(Default)]
pub struct Phaser {
    zl: [f64; 6],
    zr: [f64; 6],
    fs: f64,
    phase: f64,
    fbl: f64,
    fbr: f64,
    fade: f64,
    fade_step: f64,
    al: f64,
    ar: f64,
}

impl Phaser {
    fn chain(mut x: f64, a: f64, z: &mut [f64; 6]) -> f64 {
        for zs in z.iter_mut() {
            let y = a * x + *zs;
            *zs = x - a * y;
            x = y;
        }
        x
    }
    fn ap_coef(fs: f64, f: f64) -> f64 {
        let t = (PI * f / fs).tan();
        (t - 1.0) / (t + 1.0)
    }
}

impl StereoFx for Phaser {
    fn prepare(&mut self, fs: f64, enabled: bool, _p: &[f32]) {
        self.fs = fs;
        self.zl = [0.0; 6];
        self.zr = [0.0; 6];
        self.fbl = 0.0;
        self.fbr = 0.0;
        self.fade_step = 1.0 / (fs * 0.015);
        self.fade = if enabled { 1.0 } else { 0.0 };
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        if !enabled && self.fade == 0.0 {
            return;
        }
        let fs = self.fs;
        let (rate, depth, fb, mix) = (log(p[0] as f64, 0.05, 8.0), p[1] as f64, p[2] as f64 * 0.85, p[3] as f64);
        let inc = 2.0 * PI * rate / fs;
        let t = if enabled { 1.0 } else { 0.0 };
        for i in 0..l.len() {
            self.fade = ramp(self.fade, t, self.fade_step);
            self.phase += inc;
            if self.phase > 2.0 * PI {
                self.phase -= 2.0 * PI;
            }
            if i & 7 == 0 {
                let (ml, mr) = (0.5 + 0.5 * self.phase.sin(), 0.5 + 0.5 * self.phase.cos());
                self.al = Self::ap_coef(fs, 180.0 * (4200.0f64 / 180.0).powf(ml * depth));
                self.ar = Self::ap_coef(fs, 180.0 * (4200.0f64 / 180.0).powf(mr * depth));
            }
            let yl = Self::chain(l[i] as f64 + self.fbl * fb, self.al, &mut self.zl);
            let yr = Self::chain(r[i] as f64 + self.fbr * fb, self.ar, &mut self.zr);
            self.fbl = yl + 1e-20;
            self.fbr = yr + 1e-20;
            let m = mix * self.fade;
            l[i] = (l[i] as f64 * (1.0 - 0.5 * m) + yl * 0.5 * m) as f32;
            r[i] = (r[i] as f64 * (1.0 - 0.5 * m) + yr * 0.5 * m) as f32;
        }
    }
}

/// Diffusion allpass lengths in ms, left then right: mutually prime-ish and different on each side,
/// so the smear also decorrelates the two channels.
const ECHO_AP_MS: [[f64; 4]; 2] = [[2.3, 3.7, 6.1, 9.7], [2.9, 4.3, 5.3, 9.1]];
const TAPE_DRIVE: f64 = 1.15;
/// Bias into the tape curve: a little asymmetry, so the repeats pick up even harmonics too.
const TAPE_BIAS: f64 = 0.15;
/// High-frequency pre-emphasis around the tape curve (gain above ~2.5 kHz is 1 + this).
const TAPE_EMPH: f64 = 1.6;

/// Slow random wander of the tape speed, one per channel.
struct Drift {
    cur: f64,
    mid: f64,
    target: f64,
    timer: f64,
    rng: Rng,
}

impl Drift {
    fn new(seed: u64) -> Self {
        Drift { cur: 0.0, mid: 0.0, target: 0.0, timer: 0.0, rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }
    }
    #[inline]
    fn step(&mut self, fs: f64, slew: f64) -> f64 {
        self.timer -= 1.0;
        if self.timer <= 0.0 {
            self.target = self.rng.next_f64() * 2.0 - 1.0;
            self.timer = fs * (0.4 + 0.8 * self.rng.next_f64());
        }
        // two cascaded smoothers: the speed (= pitch) glides, it never steps
        self.mid += slew * (self.target - self.mid);
        self.cur += slew * (self.mid - self.cur);
        self.cur
    }
}

/// One channel of the echo's playback path. Everything read off the delay line goes through it
/// before it is heard or fed back, so the first repeat is already "tape" and each further pass
/// gets darker, softer and more diffuse: tone filters, low cut with a small head bump, an
/// asymmetric tape curve inside a pre/de-emphasis pair (loud highs compress first) and four
/// allpass diffusers.
struct TapeHead {
    lp: OnePole,
    lp2: OnePole,
    hp: OnePole,
    bump: Biquad,
    emph_a: f64,
    pre_z: f64,
    de_z: f64,
    sat_off: f64,
    sat_norm: f64,
    aps: Vec<DelayLine>,
    ap_len: [usize; 4],
    /// Total allpass delay in samples; the main line is shortened by it so TIME stays true.
    ap_sum: f64,
    drift: Drift,
}

impl TapeHead {
    fn new(seed: u64) -> Self {
        let off = TAPE_BIAS.tanh();
        TapeHead {
            lp: OnePole::default(),
            lp2: OnePole::default(),
            hp: OnePole::default(),
            bump: Biquad::default(),
            emph_a: 0.0,
            pre_z: 0.0,
            de_z: 0.0,
            sat_off: off,
            sat_norm: 1.0 / (TAPE_DRIVE * (1.0 - off * off)),
            aps: Vec::new(),
            ap_len: [1; 4],
            ap_sum: 4.0,
            drift: Drift::new(seed),
        }
    }

    fn prepare(&mut self, fs: f64, side: usize) {
        self.hp.set_low_pass(fs, 95.0);
        // with the 95 Hz low cut the net stays below unity everywhere, so the loop cannot run away
        self.bump.peak(fs, 105.0, 0.8, 1.5);
        self.emph_a = 1.0 - (-2.0 * PI * 2500.0 / fs).exp();
        for (len, ms) in self.ap_len.iter_mut().zip(ECHO_AP_MS[side]) {
            *len = ((ms * fs / 1000.0) as usize).max(1);
        }
        self.aps = self.ap_len.iter().map(|&n| DelayLine::new(n + 4)).collect();
        self.ap_sum = self.ap_len.iter().sum::<usize>() as f64;
    }

    fn set_tone(&mut self, fs: f64, tone: f64) {
        self.lp.set_low_pass(fs, tone);
        self.lp2.set_low_pass(fs, tone * 1.5);
    }

    #[inline]
    fn tick(&mut self, x: f64, g: f64) -> f64 {
        let x = self.lp2.low_pass(self.lp.low_pass(x));
        let x = self.bump.process(self.hp.high_pass(x));
        // pre-emphasis, tape curve, exactly inverse de-emphasis: transparent when quiet
        let k = TAPE_EMPH * (1.0 - self.emph_a);
        let e = x * (1.0 + k) - k * self.pre_z;
        self.pre_z += self.emph_a * (x - self.pre_z);
        let s = ((TAPE_DRIVE * e + TAPE_BIAS).tanh() - self.sat_off) * self.sat_norm;
        let mut y = (s + k * self.de_z) / (1.0 + k);
        self.de_z += self.emph_a * (y - self.de_z);
        for (d, &len) in self.aps.iter_mut().zip(&self.ap_len) {
            y = allpass(d, len, g, y);
        }
        y
    }
}

/// Stereo tape style delay: gliding time changes, wow, flutter and random speed drift (independent
/// per side), repeats that pass through a tape head model (`TapeHead`) and bloom into a wash as
/// DIFFUSE comes up, ping-pong spread and ducking. Repeats ring out when switched off.
/// (Tap tempo lives in the UI: it just sets the TIME knob.)
pub struct Delay {
    dl: DelayLine,
    dr: DelayLine,
    hl: TapeHead,
    hr: TapeHead,
    fs: f64,
    t_sm: f64,
    g_sm: f64,
    ws: f64,
    wcs: f64,
    fsn: f64,
    fcs: f64,
    env: f64,
    feed: f64,
    env_atk: f64,
    env_rel: f64,
    quiet: usize,
}

impl Delay {
    fn new() -> Self {
        Delay {
            dl: DelayLine::new(1),
            dr: DelayLine::new(1),
            hl: TapeHead::new(41),
            hr: TapeHead::new(67),
            fs: 48000.0,
            t_sm: 0.0,
            g_sm: 0.0,
            ws: 0.0,
            wcs: 1.0,
            fsn: 0.0,
            fcs: 1.0,
            env: 0.0,
            feed: 0.0,
            env_atk: 0.0,
            env_rel: 0.0,
            quiet: 0,
        }
    }
}

/// DIFFUSE knob to allpass coefficient.
fn echo_diffusion(v: f32) -> f64 {
    v as f64 * 0.7
}

impl StereoFx for Delay {
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]) {
        self.fs = fs;
        self.dl = DelayLine::new((fs * 1.9) as usize);
        self.dr = DelayLine::new((fs * 1.9) as usize);
        self.hl.prepare(fs, 0);
        self.hr.prepare(fs, 1);
        self.t_sm = delay_time_ms(p[0]) * fs / 1000.0;
        self.g_sm = echo_diffusion(p[7]);
        self.env_atk = coef(fs, 0.005);
        self.env_rel = coef(fs, 0.3);
        self.feed = if enabled { 1.0 } else { 0.0 };
        self.quiet = 0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        let fs = self.fs;
        if !enabled && self.feed == 0.0 && self.quiet as f64 > fs * 2.0 {
            return; // fully decayed, sleep
        }
        let target = delay_time_ms(p[0]) * fs / 1000.0;
        let (fb, mix, md, s, duck) = (p[1] as f64 * 0.97, p[2] as f64 * 1.1, p[4] as f64, p[5] as f64, p[6] as f64);
        let g_target = echo_diffusion(p[7]);
        let tone = log(p[3] as f64, 1200.0, 14000.0);
        self.hl.set_tone(fs, tone);
        self.hr.set_tone(fs, tone);
        let (glide, feed_step, drift_slew) = (coef(fs, 0.12), 1.0 / (fs * 0.02), coef(fs, 0.35));
        let (wc1, ws1) = ((2.0 * PI * 0.55 / fs).cos(), (2.0 * PI * 0.55 / fs).sin());
        let (fc1, fs1) = ((2.0 * PI * 5.7 / fs).cos(), (2.0 * PI * 5.7 / fs).sin());
        // renormalise the LFO phasors per block
        let n = (self.ws * self.ws + self.wcs * self.wcs).sqrt();
        if n > 0.0 {
            self.ws /= n;
            self.wcs /= n;
        } else {
            self.wcs = 1.0;
        }
        let n = (self.fsn * self.fsn + self.fcs * self.fcs).sqrt();
        if n > 0.0 {
            self.fsn /= n;
            self.fcs /= n;
        } else {
            self.fcs = 1.0;
        }
        let (wow_d, flut_d, drift_d) = (md * 0.003 * fs, md * 0.0003 * fs, md * 0.0025 * fs);
        let ft = if enabled { 1.0 } else { 0.0 };
        let mut peak: f64 = 0.0;
        for i in 0..l.len() {
            self.feed = ramp(self.feed, ft, feed_step);
            self.t_sm += glide * (target - self.t_sm);
            self.g_sm += glide * (g_target - self.g_sm);
            let t1 = self.ws * wc1 + self.wcs * ws1;
            self.wcs = self.wcs * wc1 - self.ws * ws1;
            self.ws = t1; // rotate wow phasor
            let t2 = self.fsn * fc1 + self.fcs * fs1;
            self.fcs = self.fcs * fc1 - self.fsn * fs1;
            self.fsn = t2; // rotate flutter phasor
            // the right side runs a quarter cycle behind and has its own drift: the image moves
            // instead of the two sides pulling against each other
            let ml = wow_d * self.ws + flut_d * self.fsn + drift_d * self.hl.drift.step(fs, drift_slew);
            let mr = 0.9 * (wow_d * self.wcs + flut_d * self.fcs) + drift_d * self.hr.drift.step(fs, drift_slew);
            let rl = self.dl.read(self.t_sm - self.hl.ap_sum + ml);
            let rr = self.dr.read(self.t_sm - self.hr.ap_sum + mr + 0.0007 * fs * s);
            let ql = self.hl.tick(rl, self.g_sm);
            let qr = self.hr.tick(rr, self.g_sm);

            let (dry_l, dry_r) = (l[i] as f64, r[i] as f64);
            let mono = 0.5 * (dry_l + dry_r);
            let in_l = self.feed * ((1.0 - s) * dry_l + s * mono);
            let in_r = self.feed * (1.0 - s) * dry_r;
            self.dl.write(in_l + fb * ((1.0 - s) * ql + s * qr) + 1e-20);
            self.dr.write(in_r + fb * ((1.0 - s) * qr + s * ql) + 1e-20);

            let am = mono.abs();
            self.env += (if am > self.env { self.env_atk } else { self.env_rel }) * (am - self.env);
            let wet = mix / (1.0 + duck * 25.0 * self.env);
            l[i] = (dry_l + ql * wet) as f32;
            r[i] = (dry_r + qr * wet) as f32;
            peak = peak.max(ql.abs() + qr.abs());
        }
        self.quiet = if peak < 1e-5 { self.quiet + l.len() } else { 0 };
    }
}

#[cfg(test)]
mod echo_tests {
    use super::*;

    fn run(p: [f32; 8], seconds: f64, input: impl Fn(usize) -> f32) -> (Vec<f32>, Vec<f32>) {
        let fs = 48000.0;
        let mut d = Delay::new();
        d.prepare(fs, true, &p);
        let n = (fs * seconds) as usize;
        let (mut l, mut r): (Vec<f32>, Vec<f32>) = ((0..n).map(&input).collect(), (0..n).map(&input).collect());
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            d.process(a, b, &p, true);
        }
        (l, r)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, v| m.max(v.abs()))
    }

    /// With DIFFUSE and MOD at zero the first repeat lands on TIME (the allpass delay is taken out
    /// of the main line; what is left is the few samples of filter lag).
    #[test]
    fn first_repeat_is_on_time() {
        let time = 0.5f32;
        let p = [time, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let (l, _) = run(p, 1.0, |i| if i == 0 { 0.5 } else { 0.0 });
        let at = (1..l.len()).max_by(|&a, &b| l[a].abs().total_cmp(&l[b].abs())).unwrap();
        let want = delay_time_ms(time) * 48.0;
        assert!((at as f64 - want).abs() < 12.0, "repeat at {at}, wanted {want}");
    }

    /// Full feedback, full diffusion, loud input: the repeats must still die away, and the tape
    /// curve must not leave the loop sitting on a DC offset.
    #[test]
    fn full_feedback_decays() {
        let p = [0.45, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 1.0];
        let burst = |i: usize| if i < 9600 { (i as f32 * 0.03).sin() * 0.9 } else { 0.0 };
        let (l, r) = run(p, 40.0, burst);
        let fs = 48000;
        let early = peak(&l[fs..3 * fs]).max(peak(&r[fs..3 * fs]));
        let late = peak(&l[38 * fs..]).max(peak(&r[38 * fs..]));
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(early > 0.05, "no repeats: {early}");
        assert!(late < early * 0.2, "not decaying: {early} -> {late}");
    }

    /// DIFFUSE spreads a click out in time and makes the two sides differ.
    #[test]
    fn diffusion_smears_and_widens() {
        let click = |i: usize| if i == 0 { 0.5 } else { 0.0 };
        let (dry_l, dry_r) = run([0.5, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0], 1.0, click);
        let (wet_l, wet_r) = run([0.5, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0], 1.0, click);
        assert!(peak(&wet_l[1..]) < peak(&dry_l[1..]) * 0.6);
        let diff = |a: &[f32], b: &[f32]| a.iter().zip(b).fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(diff(&dry_l, &dry_r) < 1e-6);
        assert!(diff(&wet_l, &wet_r) > 0.01);
    }
}

/// Octave-up pitch shifter for the reverb shimmer. Two crossfaded grains read the input at double
/// speed; each new grain starts from the point (within a 25 ms search) where the waveform best
/// matches the grain already playing, so the two stay in phase and the octave comes out steady
/// instead of warbling at the grain rate.
struct OctaveUp {
    d: DelayLine,
    grain: f64,
    search: usize,
    span: usize,
    phase: f64,
    /// cos/sin of 2*pi*phase, kept by rotation so the grain windows need no trig per sample
    win: (f64, f64),
    rot: (f64, f64),
    off: [f64; 2],
}

impl OctaveUp {
    fn new(fs: f64, grain_s: f64) -> Self {
        let (grain, search) = (fs * grain_s, (fs * 0.025) as usize);
        OctaveUp { d: DelayLine::new(grain as usize + search + 8), grain, search, span: (grain * 0.4).min(512.0) as usize, phase: 0.0, win: (1.0, 0.0), rot: ((2.0 * PI / grain).cos(), (2.0 * PI / grain).sin()), off: [0.0; 2] }
    }

    fn process(&mut self, x: f64) -> f64 {
        self.d.write(x);
        let step = 1.0 / self.grain;
        self.phase += step;
        self.win = (self.win.0 * self.rot.0 - self.win.1 * self.rot.1, self.win.1 * self.rot.0 + self.win.0 * self.rot.1);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.win = ((2.0 * PI * self.phase).cos(), (2.0 * PI * self.phase).sin());
        }
        let mut y = 0.0;
        for k in 0..2 {
            let mut ph = self.phase + 0.5 * k as f64;
            if ph >= 1.0 {
                ph -= 1.0;
            }
            // the read point closes in by one sample per sample = double speed = one octave up
            let base = (1.0 - ph) * self.grain + 2.0;
            if ph < step {
                // this grain is silent right now and the other one is at full level: restart in phase with it
                self.off[k] = self.align(base, base - 0.5 * self.grain + self.off[1 - k]);
            }
            // Hann windows half a grain apart: they always add up to one
            let w = if k == 0 { 0.5 - 0.5 * self.win.0 } else { 0.5 + 0.5 * self.win.0 };
            y += self.d.read(base + self.off[k]) * w;
        }
        y
    }

    /// Extra delay for a grain starting at delay `base` that makes what it is about to play line up
    /// with what the grain at delay `other` is about to play (best normalized correlation).
    fn align(&self, base: f64, other: f64) -> f64 {
        let (a0, b0) = (base as usize, other as usize);
        let score = |off: usize| {
            let (mut c, mut e) = (0.0, 1e-12);
            let mut m = 0;
            while m < self.span {
                let a = self.d.tap(a0 + off - m);
                c += a * self.d.tap(b0 - m);
                e += a * a;
                m += 4;
            }
            c / e.sqrt()
        };
        let (mut best, mut best_off) = (f64::MIN, 0);
        for off in (0..=self.search).step_by(4) {
            let sc = score(off);
            if sc > best {
                (best, best_off) = (sc, off);
            }
        }
        for off in best_off.saturating_sub(3)..=(best_off + 3).min(self.search) {
            let sc = score(off);
            if sc > best {
                (best, best_off) = (sc, off);
            }
        }
        best_off as f64
    }
}

/// Schroeder allpass on a fixed whole-sample delay.
#[inline]
fn allpass(d: &mut DelayLine, len: usize, g: f64, x: f64) -> f64 {
    let z = d.tap(len);
    let w = x + g * z;
    d.write(w);
    z - g * w
}

const TANK: usize = 16;
const LINE_MS: [f64; TANK] = [23.3, 27.1, 29.9, 33.7, 37.3, 41.9, 44.3, 48.1, 53.3, 57.7, 61.9, 67.1, 71.3, 77.9, 83.9, 89.3];
const TANK_AP_MS: [f64; TANK] = [5.3, 3.7, 7.1, 4.3, 8.9, 6.1, 10.7, 4.9, 11.9, 7.7, 9.5, 5.9, 12.7, 8.3, 6.7, 10.1];
const MOD_HZ: [f64; TANK] = [0.19, 0.27, 0.31, 0.37, 0.43, 0.49, 0.57, 0.63, 0.69, 0.77, 0.83, 0.91, 0.97, 1.03, 1.09, 1.21];
/// Input diffusers: four for the left input, then four for the right (samples at 44.1 kHz).
const DIFF_SAMPLES: [usize; 8] = [142, 107, 379, 277, 151, 113, 359, 293];
/// Diffusers on the shimmer return: two left, two right.
const SHIM_DIFF_MS: [f64; 4] = [13.7, 19.3, 15.1, 21.7];
const TANK_AP_G: f64 = 0.5;
const REVERB_OUT: f64 = 0.66;
const SHIM_K: f64 = 0.8;
const SHIM_LIMIT: f64 = 0.3;

/// Lush stereo hall. Stereo in, per-channel pre-delay and input diffusion, then a 16-line
/// feedback delay network (Hadamard mixing) with an allpass diffuser, high damping and a separate
/// low-band decay in every line; line lengths wander with a sine plus a slow random drift.
/// Shimmer: the tank output goes through a stereo octave-up shifter and back into the tank, so
/// every trip around adds another, softer octave.
pub struct Reverb {
    lines: Vec<DelayLine>,
    aps: Vec<DelayLine>,
    ap_len: [usize; TANK],
    pre: Vec<DelayLine>,
    diff: Vec<DelayLine>,
    diff_len: [usize; 8],
    sdiff: Vec<DelayLine>,
    sdiff_len: [usize; 4],
    damp: [OnePole; TANK],
    bass: [OnePole; TANK],
    gains: [f64; TANK],
    shelf: [f64; TANK],
    v: [f64; TANK],
    ms: [f64; TANK],
    mc: [f64; TANK],
    rc: [f64; TANK],
    rs: [f64; TANK],
    rnd_target: [f64; TANK],
    rnd_mid: [f64; TANK],
    rnd: [f64; TANK],
    rnd_timer: [f64; TANK],
    rng: Rng,
    wet_hp: [OnePole; 2],
    shim: Vec<OctaveUp>,
    shim_hp: [OnePole; 2],
    shim_lp: [OnePole; 2],
    shim_fb: [f64; 2],
    shim_env: f64,
    fs: f64,
    scale: f64,
    feed: f64,
    quiet: usize,
}

impl Reverb {
    fn new() -> Self {
        Reverb {
            lines: Vec::new(),
            aps: Vec::new(),
            ap_len: [1; TANK],
            pre: Vec::new(),
            diff: Vec::new(),
            diff_len: [1; 8],
            sdiff: Vec::new(),
            sdiff_len: [1; 4],
            damp: [OnePole::default(); TANK],
            bass: [OnePole::default(); TANK],
            gains: [0.0; TANK],
            shelf: [0.0; TANK],
            v: [0.0; TANK],
            ms: [0.0; TANK],
            mc: [0.0; TANK],
            rc: [0.0; TANK],
            rs: [0.0; TANK],
            rnd_target: [0.0; TANK],
            rnd_mid: [0.0; TANK],
            rnd: [0.0; TANK],
            rnd_timer: [0.0; TANK],
            rng: Rng(0x5EED_CA7E_D2A1_0001),
            wet_hp: [OnePole::default(); 2],
            shim: Vec::new(),
            shim_hp: [OnePole::default(); 2],
            shim_lp: [OnePole::default(); 2],
            shim_fb: [0.0; 2],
            shim_env: 0.0,
            fs: 48000.0,
            scale: 1.0,
            feed: 0.0,
            quiet: 0,
        }
    }
}

impl StereoFx for Reverb {
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]) {
        self.fs = fs;
        self.lines = (0..TANK).map(|i| DelayLine::new((LINE_MS[i] * 1.75 * fs / 1000.0 + fs * 0.004) as usize)).collect();
        for i in 0..TANK {
            self.ap_len[i] = (TANK_AP_MS[i] * fs / 1000.0) as usize;
        }
        self.aps = (0..TANK).map(|i| DelayLine::new(self.ap_len[i] + 4)).collect();
        self.pre = (0..2).map(|_| DelayLine::new((fs * 0.21) as usize)).collect();
        for i in 0..8 {
            self.diff_len[i] = (DIFF_SAMPLES[i] as f64 * fs / 44100.0) as usize;
        }
        self.diff = (0..8).map(|i| DelayLine::new(self.diff_len[i] + 4)).collect();
        for i in 0..4 {
            self.sdiff_len[i] = (SHIM_DIFF_MS[i] * fs / 1000.0) as usize;
        }
        self.sdiff = (0..4).map(|i| DelayLine::new(self.sdiff_len[i] + 4)).collect();
        self.shim = vec![OctaveUp::new(fs, 0.089), OctaveUp::new(fs, 0.101)];
        self.damp = [OnePole::default(); TANK];
        self.bass = [OnePole::default(); TANK];
        self.wet_hp = [OnePole::default(); 2];
        self.shim_hp = [OnePole::default(); 2];
        self.shim_lp = [OnePole::default(); 2];
        for c in 0..2 {
            self.wet_hp[c].set_low_pass(fs, 90.0);
            self.shim_hp[c].set_low_pass(fs, 250.0);
            self.shim_lp[c].set_low_pass(fs, 6000.0);
        }
        for f in self.bass.iter_mut() {
            f.set_low_pass(fs, 250.0);
        }
        self.ms = [0.0; TANK];
        self.mc = [0.0; TANK];
        self.rnd_target = [0.0; TANK];
        self.rnd_mid = [0.0; TANK];
        self.rnd = [0.0; TANK];
        self.rnd_timer = [0.0; TANK];
        self.scale = 0.45 + 1.2 * p[2] as f64;
        self.feed = if enabled { 1.0 } else { 0.0 };
        self.quiet = 0;
        self.shim_fb = [0.0; 2];
        self.shim_env = 0.0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        let fs = self.fs;
        if !enabled && self.feed == 0.0 && self.quiet as f64 > fs * 3.0 {
            return;
        }
        let t60 = reverb_decay_s(p[0]);
        // lows ring a little longer than the mids in short rooms, shorter in huge ones (warm, never muddy)
        let t60_low = t60 * (1.35 - 0.6 * p[0] as f64);
        let pre_d = (p[1] as f64 * 0.2 * fs).max(1.0);
        let target_scale = 0.45 + 1.2 * p[2] as f64;
        let (damp_hz, mod_d) = (log(p[3] as f64, 1500.0, 15000.0), p[4] as f64 * 0.0016 * fs);
        let (shim_amt, mix) = (p[5] as f64, p[6] as f64 * 1.2);
        for i in 0..TANK {
            self.damp[i].set_low_pass(fs, damp_hz);
            let len = LINE_MS[i] * self.scale * fs / 1000.0 + self.ap_len[i] as f64;
            self.gains[i] = 10f64.powf(-3.0 * len / (fs * t60));
            self.shelf[i] = 10f64.powf(-3.0 * len / (fs * t60_low)) / self.gains[i] - 1.0;
        }
        // the tank gets louder as the decay gets longer: scale the shimmer send to match, so the knob feels the same at any decay
        let shim_gain = shim_amt * SHIM_K * (self.scale / t60).sqrt();
        let (feed_step, glide) = (1.0 / (fs * 0.02), 1.0 - (-1.0 / (fs * 0.25)).exp());
        let rnd_slew = 1.0 - (-1.0 / (fs * 0.12)).exp();
        let (env_atk, env_rel) = (1.0 - (-1.0 / (fs * 0.005)).exp(), 1.0 - (-1.0 / (fs * 0.3)).exp());
        for i in 0..TANK {
            let nrm = (self.ms[i] * self.ms[i] + self.mc[i] * self.mc[i]).sqrt();
            if nrm > 0.0 {
                self.ms[i] /= nrm;
                self.mc[i] /= nrm;
            } else {
                self.ms[i] = (i as f64 * 0.7).sin();
                self.mc[i] = (i as f64 * 0.7).cos();
            }
            self.rc[i] = (2.0 * PI * MOD_HZ[i] / fs).cos();
            self.rs[i] = (2.0 * PI * MOD_HZ[i] / fs).sin();
        }
        let ft = if enabled { 1.0 } else { 0.0 };
        let mut peak: f64 = 0.0;
        for n in 0..l.len() {
            self.feed = ramp(self.feed, ft, feed_step);
            self.scale += glide * (target_scale - self.scale);

            // pre-delay, shimmer return and input diffusion, per channel
            let mut x = [l[n] as f64 * self.feed, r[n] as f64 * self.feed];
            for c in 0..2 {
                self.pre[c].write(x[c]);
                x[c] = self.pre[c].read(pre_d) + self.shim_fb[c];
                for k in 0..4 {
                    x[c] = allpass(&mut self.diff[4 * c + k], self.diff_len[4 * c + k], 0.62, x[c]);
                }
            }

            let len_scale = self.scale * fs / 1000.0;
            let mut out = [0.0f64; 2];
            for i in 0..TANK {
                let t = self.ms[i] * self.rc[i] + self.mc[i] * self.rs[i];
                self.mc[i] = self.mc[i] * self.rc[i] - self.ms[i] * self.rs[i];
                self.ms[i] = t;
                self.rnd_timer[i] -= 1.0;
                if self.rnd_timer[i] <= 0.0 {
                    self.rnd_target[i] = self.rng.next_f64() * 2.0 - 1.0;
                    self.rnd_timer[i] = fs * (0.2 + 0.5 * self.rng.next_f64());
                }
                self.rnd_mid[i] += rnd_slew * (self.rnd_target[i] - self.rnd_mid[i]);
                self.rnd[i] += rnd_slew * (self.rnd_mid[i] - self.rnd[i]);

                let d = LINE_MS[i] * len_scale + mod_d * (1.0 + 0.6 * self.ms[i] + 0.4 * self.rnd[i]);
                let y = self.damp[i].low_pass(self.lines[i].read(d)) * self.gains[i];
                let y = y + self.shelf[i] * self.bass[i].low_pass(y);
                let y = allpass(&mut self.aps[i], self.ap_len[i], TANK_AP_G, y);
                self.v[i] = y;
                // even lines feed the left output, odd lines the right
                out[i & 1] += if i & 2 == 0 { y } else { -y };
            }

            // 16-point fast Walsh-Hadamard mix (energy preserving)
            let v = &mut self.v;
            let mut h = 1;
            while h < TANK {
                let mut i = 0;
                while i < TANK {
                    for j in i..i + h {
                        let (a, b) = (v[j], v[j + h]);
                        v[j] = a + b;
                        v[j + h] = a - b;
                    }
                    i += h << 1;
                }
                h <<= 1;
            }
            for i in 0..TANK {
                let inj = x[i & 1] * if i & 4 == 0 { 0.35 } else { -0.35 };
                self.lines[i].write(v[i] * 0.25 + inj + 1e-20);
            }

            // shimmer: octave-up of each side, band-limited and diffused, returns to the opposite side
            if shim_amt > 0.0 {
                let mut s = [0.0f64; 2];
                for c in 0..2 {
                    let y = self.shim[c].process(out[c]);
                    let y = self.shim_lp[c].low_pass(self.shim_hp[c].high_pass(y));
                    let y = allpass(&mut self.sdiff[2 * c], self.sdiff_len[2 * c], 0.6, y);
                    s[c] = shim_gain * allpass(&mut self.sdiff[2 * c + 1], self.sdiff_len[2 * c + 1], 0.6, y);
                }
                // transparent until the return gets loud, then it is held at a ceiling
                let a = s[0].abs().max(s[1].abs());
                self.shim_env += (if a > self.shim_env { env_atk } else { env_rel }) * (a - self.shim_env);
                let g = SHIM_LIMIT / self.shim_env.max(SHIM_LIMIT);
                self.shim_fb = [2.0 * (0.5 * g * s[1]).tanh(), 2.0 * (0.5 * g * s[0]).tanh()];
            } else {
                self.shim_fb = [0.0; 2];
            }

            let wl = self.wet_hp[0].high_pass(out[0]) * REVERB_OUT;
            let wr = self.wet_hp[1].high_pass(out[1]) * REVERB_OUT;
            let dry = 1.0 - 0.25 * mix * self.feed;
            l[n] = (l[n] as f64 * dry + wl * mix) as f32;
            r[n] = (r[n] as f64 * dry + wr * mix) as f32;
            peak = peak.max(wl.abs() + wr.abs());
        }
        self.quiet = if peak < 1e-5 { self.quiet + l.len() } else { 0 };
    }
}

#[cfg(test)]
mod reverb_tests {
    use super::*;

    const FS: f64 = 48000.0;

    /// Runs the reverb (knobs: DECAY, PREDELAY, SIZE, TONE, MOD, SHIMMER, MIX) on a mono input
    /// padded to `secs` and returns only the wet signal.
    fn wet(p: [f32; 7], input: &[f32], secs: f64) -> (Vec<f64>, Vec<f64>) {
        let n = (secs * FS) as usize;
        let mut dry = vec![0.0f32; n];
        let m = input.len().min(n);
        dry[..m].copy_from_slice(&input[..m]);
        let (mut l, mut r) = (dry.clone(), dry.clone());
        let mut fx = make_post("reverb");
        fx.prepare(FS, true, &p);
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            fx.process(a, b, &p, true);
        }
        let mix = p[6] as f64 * 1.2;
        let f = |o: &[f32]| o.iter().zip(&dry).map(|(&o, &d)| (o as f64 - d as f64 * (1.0 - 0.25 * mix)) / mix).collect::<Vec<f64>>();
        (f(&l), f(&r))
    }

    fn noise(secs: f64, amp: f64) -> Vec<f32> {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        (0..(secs * FS) as usize).map(|_| (amp * (rng.next_f64() * 2.0 - 1.0)) as f32).collect()
    }

    fn tone(hz: f64, secs: f64, amp: f64) -> Vec<f32> {
        let n = (secs * FS) as usize;
        (0..n)
            .map(|i| {
                let fade = (i.min(n - 1 - i) as f64 / (0.01 * FS)).min(1.0);
                (amp * fade * (2.0 * PI * hz * i as f64 / FS).sin()) as f32
            })
            .collect()
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// Decay time from the Schroeder integral (-5 to -35 dB, doubled), starting at sample `from`.
    fn t60(l: &[f64], r: &[f64], from: usize) -> f64 {
        let mut edc: Vec<f64> = (from..l.len()).map(|i| l[i] * l[i] + r[i] * r[i]).collect();
        for i in (0..edc.len() - 1).rev() {
            edc[i] += edc[i + 1];
        }
        let at = |db: f64| edc.iter().position(|&e| 10.0 * (e / edc[0]).log10() <= db).unwrap_or(edc.len()) as f64;
        2.0 * (at(-35.0) - at(-5.0)) / FS
    }

    /// Hann-windowed amplitude of the `hz` component.
    fn partial(x: &[f64], hz: f64) -> f64 {
        let (mut re, mut im, mut ws) = (0.0, 0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / x.len() as f64).cos();
            let ph = 2.0 * PI * hz * i as f64 / FS;
            re += w * v * ph.cos();
            im += w * v * ph.sin();
            ws += w;
        }
        2.0 * (re * re + im * im).sqrt() / ws
    }

    /// Level of the band around `hz` (40 ms windows) and how much it flutters (0 = perfectly steady).
    fn band(x: &[f64], hz: f64) -> (f64, f64) {
        let (hop, win) = (240, 1920);
        let env: Vec<f64> = (0..(x.len() - win) / hop).map(|k| partial(&x[k * hop..k * hop + win], hz)).collect();
        let mut acc = 0.0;
        let mut cnt = 0.0;
        for k in 30..env.len() - 30 {
            let ma = env[k - 30..=k + 30].iter().sum::<f64>() / 61.0;
            acc += (env[k] / ma - 1.0).powi(2);
            cnt += 1.0;
        }
        (rms(&env), (acc / cnt).sqrt())
    }

    fn kurtosis(x: &[f64]) -> f64 {
        let m2 = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
        let m4 = x.iter().map(|v| v.powi(4)).sum::<f64>() / x.len() as f64;
        m4 / (m2 * m2)
    }

    fn corr(l: &[f64], r: &[f64]) -> f64 {
        let lr: f64 = l.iter().zip(r).map(|(a, b)| a * b).sum();
        lr / (l.iter().map(|v| v * v).sum::<f64>() * r.iter().map(|v| v * v).sum::<f64>()).sqrt()
    }

    /// The previous shimmer shifter (two free-running 60 ms grains), kept here for comparison.
    fn old_shifter(x: &[f64]) -> Vec<f64> {
        let g = FS * 0.06;
        let mut d = DelayLine::new(g as usize + 8);
        let mut phase = 0.0f64;
        x.iter()
            .map(|&v| {
                d.write(v);
                phase += 1.0 / g;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                let p2 = (phase + 0.5) % 1.0;
                let (w1, w2) = ((PI * phase).sin(), (PI * p2).sin());
                d.read((1.0 - phase) * g + 2.0) * w1 * w1 + d.read((1.0 - p2) * g + 2.0) * w2 * w2
            })
            .collect()
    }

    #[test]
    fn reverb_decay_follows_knob() {
        for decay in [0.2f32, 0.45] {
            let want = reverb_decay_s(decay);
            let (l, r) = wet([decay, 0.0, 0.6, 0.5, 0.4, 0.0, 1.0], &noise(0.05, 0.5), 0.5 + want * 2.2);
            let got = t60(&l, &r, (0.3 * FS) as usize);
            assert!((got / want - 1.0).abs() < 0.15, "knob says {want:.2}s, tail measures {got:.2}s");
        }
    }

    #[test]
    fn reverb_is_wide_and_silent_without_input() {
        let (l, r) = wet([0.45, 0.15, 0.6, 0.5, 0.4, 0.5, 1.0], &[], 1.0);
        assert!(l.iter().chain(&r).all(|v| v.abs() < 1e-9));
        let (l, r) = wet([0.45, 0.15, 0.6, 0.5, 0.4, 0.0, 1.0], &noise(0.05, 0.5), 3.0);
        let from = (0.3 * FS) as usize;
        assert!(corr(&l[from..], &r[from..]).abs() < 0.3);
    }

    #[test]
    fn reverb_stays_bounded_when_maxed() {
        let s = |t: f64| (t * FS) as usize;
        let (l, r) = wet([1.0; 7], &noise(6.0, 1.0), 20.0);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 10.0));
        assert!(rms(&l[s(18.0)..]) < rms(&l[s(6.0)..s(8.0)]));
    }

    #[test]
    fn shimmer_octave_is_steady() {
        // 345 Hz and 587 Hz made the old free-running shifter warble badly
        for hz in [110.0, 345.0, 587.0] {
            let mut sh = OctaveUp::new(FS, 0.089);
            let y: Vec<f64> = tone(hz, 2.0, 0.3).iter().map(|&v| sh.process(v as f64)).collect();
            let (level, flutter) = band(&y[(0.5 * FS) as usize..(1.8 * FS) as usize], 2.0 * hz);
            assert!(level > 0.27 && flutter < 0.05, "{hz} Hz: level {level:.3}, flutter {flutter:.3}");
        }
    }

    #[test]
    fn shimmer_knob_adds_the_octave() {
        let s = |t: f64| (t * FS) as usize;
        let octave = |amount: f32| {
            let (l, _) = wet([0.85, 0.0, 0.9, 0.5, 0.4, amount, 1.0], &tone(330.0, 1.0, 0.3), 5.0);
            band(&l[s(1.5)..s(4.5)], 660.0).0 / band(&l[s(1.5)..s(4.5)], 330.0).0
        };
        assert!(octave(0.0) < 0.01);
        assert!(octave(1.0) > 0.3);
    }

    #[test]
    #[ignore]
    fn reverb_speed() {
        let input = noise(5.0, 0.3);
        for (name, shimmer) in [("no shimmer", 0.0f32), ("shimmer", 0.5)] {
            let best = (0..8)
                .map(|_| {
                    let t0 = std::time::Instant::now();
                    let _ = wet([0.45, 0.15, 0.6, 0.5, 0.4, shimmer, 0.3], &input, 5.0);
                    t0.elapsed().as_secs_f64()
                })
                .fold(f64::MAX, f64::min);
            println!("{name}: {:.0} ns/sample = {:.1}% of one core at 48 kHz", best * 1e9 / (5.0 * FS), best / 5.0 * 100.0);
        }
    }

    #[test]
    #[ignore]
    fn reverb_report() {
        let s = |t: f64| (t * FS) as usize;
        let burst = noise(0.05, 0.5);
        for (name, p) in [
            ("default", [0.45, 0.15, 0.6, 0.5, 0.4, 0.0, 1.0f32]),
            ("short  ", [0.2, 0.05, 0.5, 0.5, 0.2, 0.0, 1.0]),
            ("long   ", [0.85, 0.3, 0.9, 0.5, 0.4, 0.0, 1.0]),
            ("small  ", [0.45, 0.0, 0.0, 0.5, 0.4, 0.0, 1.0]),
            ("dark   ", [0.45, 0.15, 0.6, 0.0, 0.4, 0.0, 1.0]),
        ] {
            let want = reverb_decay_s(p[0]);
            let (l, r) = wet(p, &burst, 0.5 + want * 2.2);
            println!(
                "{name} knob {:5.2}s  t60 {:5.2}s  wet rms {:.5}  first 0.5s rms {:.5}  L/R corr {:+.3}",
                want,
                t60(&l, &r, s(0.3)),
                rms(&l) * (l.len() as f64).sqrt() / 100.0,
                rms(&l[..s(0.5)]),
                corr(&l[s(0.3)..], &r[s(0.3)..])
            );
        }
        // band decay times: lows vs mids vs highs at the default setting
        for hz in [110.0, 440.0, 1760.0, 5000.0] {
            let (l, r) = wet([0.45, 0.0, 0.6, 0.5, 0.4, 0.0, 1.0], &tone(hz, 0.3, 0.3), 7.0);
            println!("tone {hz:6.0} Hz  t60 {:.2}s", t60(&l, &r, s(0.4)));
        }
        // echo density of the impulse response (3.0 = gaussian, i.e. perfectly smooth)
        let mut click = vec![0.0f32; 8];
        click[0] = 1.0;
        let (l, _) = wet([0.45, 0.0, 0.6, 1.0, 0.0, 0.0, 1.0], &click, 1.5);
        println!(
            "kurtosis 20-60ms {:.2}  60-150ms {:.2}  150-400ms {:.2}  400-1000ms {:.2}",
            kurtosis(&l[s(0.02)..s(0.06)]),
            kurtosis(&l[s(0.06)..s(0.15)]),
            kurtosis(&l[s(0.15)..s(0.4)]),
            kurtosis(&l[s(0.4)..s(1.0)])
        );
        // shimmer: a 330 Hz note into a long hall
        for sh in [0.0f32, 0.45, 1.0] {
            let (l, r) = wet([0.85, 0.0, 0.9, 0.5, 0.4, sh, 1.0], &tone(330.0, 1.0, 0.3), 6.0);
            let seg = &l[s(1.5)..s(4.5)];
            let b: Vec<(f64, f64)> = [330.0, 660.0, 1320.0, 2640.0, 5280.0].iter().map(|&hz| band(seg, hz)).collect();
            println!(
                "shimmer {sh:.2}: 330 {:.4}  660 {:.4}  1320 {:.4}  2640 {:.4}  5280 {:.4} | ripple 330 {:.3}  660 {:.3}  1320 {:.3} | rms {:.4} corr {:+.3}",
                b[0].0, b[1].0, b[2].0, b[3].0, b[4].0, b[0].1, b[1].1, b[2].1,
                rms(seg),
                corr(seg, &r[s(1.5)..s(4.5)])
            );
        }
        // shimmer at other decay times: the octave should sit at a similar level relative to the note
        for dec in [0.2f32, 0.45, 0.7] {
            let (l, _) = wet([dec, 0.0, 0.6, 0.5, 0.4, 1.0, 1.0], &tone(330.0, 1.0, 0.3), 3.0);
            let seg = &l[s(0.5)..s(1.5)];
            println!("shimmer 1.00 at decay {dec:.2}: 330 {:.4}  660 {:.4}  1320 {:.4}", band(seg, 330.0).0, band(seg, 660.0).0, band(seg, 1320.0).0);
        }
        // the pitch shifter on its own: steady notes in, how steady is the octave coming out?
        for hz in [110.0, 196.0, 330.0, 345.0, 440.0, 587.0, 1000.0] {
            let note: Vec<f64> = tone(hz, 3.0, 0.3).iter().map(|&v| v as f64).collect();
            let mut sh = OctaveUp::new(FS, 0.089);
            let new: Vec<f64> = note.iter().map(|&v| sh.process(v)).collect();
            let old = old_shifter(&note);
            let (o, n) = (band(&old[s(0.5)..s(2.5)], 2.0 * hz), band(&new[s(0.5)..s(2.5)], 2.0 * hz));
            println!("shifter {hz:5.0} Hz: old level {:.3} flutter {:.3} | new level {:.3} flutter {:.3}", o.0, o.1, n.0, n.1);
        }
        // a fifth (two notes at once)
        let (a, b) = (tone(220.0, 3.0, 0.2), tone(330.0, 3.0, 0.2));
        let chord: Vec<f64> = a.iter().zip(&b).map(|(&x, &y)| (x + y) as f64).collect();
        let mut sh = OctaveUp::new(FS, 0.089);
        let new: Vec<f64> = chord.iter().map(|&v| sh.process(v)).collect();
        let old = old_shifter(&chord);
        for hz in [440.0, 660.0] {
            let (o, n) = (band(&old[s(0.5)..s(2.5)], hz), band(&new[s(0.5)..s(2.5)], hz));
            println!("shifter fifth, {hz:.0} Hz: old level {:.3} flutter {:.3} | new level {:.3} flutter {:.3}", o.0, o.1, n.0, n.1);
        }
        // worst case: everything maxed, loud input
        let p = [1.0f32; 7];
        let t0 = std::time::Instant::now();
        let (l, _) = wet(p, &noise(6.0, 1.0), 20.0);
        let el = t0.elapsed().as_secs_f64();
        let peak = l.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        println!("maxed: peak {peak:.2}  rms 6-8s {:.4}  rms 18-20s {:.4}  ({:.0}x realtime)", rms(&l[s(6.0)..s(8.0)]), rms(&l[s(18.0)..s(20.0)]), 20.0 / el);
    }
}
