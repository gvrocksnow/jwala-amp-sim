//! Stomp boxes before the amp (compressor, drives, fuzz) and stereo effects after the cab.
//! Knob values arrive as normalised 0..1 slices in the order of `spec::PRE` / `spec::POST`.

use super::delay_line::DelayLine;
use super::filters::OnePole;
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

/// Stereo tape/analog style delay: gliding time changes, wow & flutter, filtered and saturated
/// repeats, ping-pong spread and ducking. Repeats ring out when switched off.
/// (Tap tempo lives in the UI: it just sets the TIME knob.)
pub struct Delay {
    dl: DelayLine,
    dr: DelayLine,
    lp_l: OnePole,
    lp_r: OnePole,
    lp2_l: OnePole,
    lp2_r: OnePole,
    hp_l: OnePole,
    hp_r: OnePole,
    fs: f64,
    t_sm: f64,
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
            lp_l: OnePole::default(),
            lp_r: OnePole::default(),
            lp2_l: OnePole::default(),
            lp2_r: OnePole::default(),
            hp_l: OnePole::default(),
            hp_r: OnePole::default(),
            fs: 48000.0,
            t_sm: 0.0,
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

impl StereoFx for Delay {
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]) {
        self.fs = fs;
        self.dl = DelayLine::new((fs * 1.9) as usize);
        self.dr = DelayLine::new((fs * 1.9) as usize);
        self.hp_l.set_low_pass(fs, 110.0);
        self.hp_r.set_low_pass(fs, 110.0);
        self.t_sm = delay_time_ms(p[0]) * fs / 1000.0;
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
        let tone = log(p[3] as f64, 1200.0, 14000.0);
        self.lp_l.set_low_pass(fs, tone);
        self.lp_r.set_low_pass(fs, tone);
        self.lp2_l.set_low_pass(fs, tone * 1.5);
        self.lp2_r.set_low_pass(fs, tone * 1.5);
        let (glide, feed_step) = (coef(fs, 0.12), 1.0 / (fs * 0.02));
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
        let (wow_d, flut_d) = (md * 0.0035 * fs, md * 0.00035 * fs);
        let ft = if enabled { 1.0 } else { 0.0 };
        let mut peak: f64 = 0.0;
        for i in 0..l.len() {
            self.feed = ramp(self.feed, ft, feed_step);
            self.t_sm += glide * (target - self.t_sm);
            let t1 = self.ws * wc1 + self.wcs * ws1;
            self.wcs = self.wcs * wc1 - self.ws * ws1;
            self.ws = t1; // rotate wow phasor
            let t2 = self.fsn * fc1 + self.fcs * fs1;
            self.fcs = self.fcs * fc1 - self.fsn * fs1;
            self.fsn = t2; // rotate flutter phasor
            let m = wow_d * self.ws + flut_d * self.fsn;
            let rl = self.dl.read(self.t_sm + m);
            let rr = self.dr.read(self.t_sm - m * 0.8 + 0.0007 * fs * s);

            // repeats get darker, thinner and gently saturated each pass
            let a = self.lp_l.low_pass(rl);
            let a = self.lp2_l.low_pass(a);
            let ql = (1.15 * self.hp_l.high_pass(a)).tanh() / 1.15;
            let b = self.lp_r.low_pass(rr);
            let b = self.lp2_r.low_pass(b);
            let qr = (1.15 * self.hp_r.high_pass(b)).tanh() / 1.15;

            let (dry_l, dry_r) = (l[i] as f64, r[i] as f64);
            let mono = 0.5 * (dry_l + dry_r);
            let in_l = self.feed * ((1.0 - s) * dry_l + s * mono);
            let in_r = self.feed * (1.0 - s) * dry_r;
            self.dl.write(in_l + fb * ((1.0 - s) * ql + s * qr) + 1e-20);
            self.dr.write(in_r + fb * ((1.0 - s) * qr + s * ql) + 1e-20);

            let am = mono.abs();
            self.env += (if am > self.env { self.env_atk } else { self.env_rel }) * (am - self.env);
            let wet = mix / (1.0 + duck * 25.0 * self.env);
            l[i] = (dry_l + rl * wet) as f32;
            r[i] = (dry_r + rr * wet) as f32;
            peak = peak.max(rl.abs() + rr.abs());
        }
        self.quiet = if peak < 1e-5 { self.quiet + l.len() } else { 0 };
    }
}

/// Octave-up pitch shifter (two crossfaded grains) used for the reverb shimmer.
struct OctaveUp {
    d: DelayLine,
    grain: usize,
    phase: f64,
}

impl OctaveUp {
    fn new(fs: f64) -> Self {
        let grain = (fs * 0.06) as usize;
        OctaveUp { d: DelayLine::new(grain + 8), grain, phase: 0.0 }
    }
    fn process(&mut self, x: f64) -> f64 {
        self.d.write(x);
        let g = self.grain as f64;
        self.phase += 1.0 / g;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        let mut p2 = self.phase + 0.5;
        if p2 >= 1.0 {
            p2 -= 1.0;
        }
        let (w1, w2) = ((PI * self.phase).sin(), (PI * p2).sin());
        self.d.read((1.0 - self.phase) * g + 2.0) * w1 * w1 + self.d.read((1.0 - p2) * g + 2.0) * w2 * w2
    }
}

/// Lush stereo hall: input diffusion, 8-line modulated feedback delay network with
/// Hadamard mixing and per-line damping, pre-delay and an octave-up shimmer loop.
pub struct Reverb {
    lines: Vec<DelayLine>,
    pre: DelayLine,
    diff: Vec<DelayLine>,
    diff_len: [usize; 4],
    damp: [OnePole; 8],
    wet_hp_l: OnePole,
    wet_hp_r: OnePole,
    shim_hp: OnePole,
    v: [f64; 8],
    ms: [f64; 8],
    mc: [f64; 8],
    rc: [f64; 8],
    rs: [f64; 8],
    gains: [f64; 8],
    shim: OctaveUp,
    fs: f64,
    scale: f64,
    feed: f64,
    shim_fb: f64,
    shim_env: f64,
    quiet: usize,
}

const BASE_MS: [f64; 8] = [31.7, 37.9, 41.3, 47.9, 53.1, 59.9, 67.3, 73.7];
const MOD_HZ: [f64; 8] = [0.31, 0.43, 0.57, 0.69, 0.83, 0.97, 1.09, 1.21];
const DIFF_SAMPLES: [usize; 4] = [142, 107, 379, 277];

impl Reverb {
    fn new() -> Self {
        Reverb {
            lines: Vec::new(),
            pre: DelayLine::new(1),
            diff: Vec::new(),
            diff_len: [1; 4],
            damp: [OnePole::default(); 8],
            wet_hp_l: OnePole::default(),
            wet_hp_r: OnePole::default(),
            shim_hp: OnePole::default(),
            v: [0.0; 8],
            ms: [0.0; 8],
            mc: [0.0; 8],
            rc: [0.0; 8],
            rs: [0.0; 8],
            gains: [0.0; 8],
            shim: OctaveUp::new(48000.0),
            fs: 48000.0,
            scale: 1.0,
            feed: 0.0,
            shim_fb: 0.0,
            shim_env: 0.0,
            quiet: 0,
        }
    }
}

impl StereoFx for Reverb {
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]) {
        self.fs = fs;
        self.lines = (0..8).map(|i| DelayLine::new((BASE_MS[i] * 1.75 * fs / 1000.0 + fs * 0.004) as usize)).collect();
        self.ms = [0.0; 8];
        self.mc = [0.0; 8];
        self.damp = [OnePole::default(); 8];
        self.pre = DelayLine::new((fs * 0.21) as usize);
        for i in 0..4 {
            self.diff_len[i] = (DIFF_SAMPLES[i] as f64 * fs / 44100.0) as usize;
        }
        self.diff = (0..4).map(|i| DelayLine::new(self.diff_len[i] + 4)).collect();
        self.wet_hp_l.set_low_pass(fs, 90.0);
        self.wet_hp_r.set_low_pass(fs, 90.0);
        self.shim_hp.set_low_pass(fs, 400.0);
        self.shim = OctaveUp::new(fs);
        self.scale = 0.45 + 1.2 * p[2] as f64;
        self.feed = if enabled { 1.0 } else { 0.0 };
        self.quiet = 0;
        self.shim_fb = 0.0;
        self.shim_env = 0.0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        let fs = self.fs;
        if !enabled && self.feed == 0.0 && self.quiet as f64 > fs * 3.0 {
            return;
        }
        let t60 = reverb_decay_s(p[0]);
        let pre_d = (p[1] as f64 * 0.2 * fs).max(1.0);
        let target_scale = 0.45 + 1.2 * p[2] as f64;
        let (damp_hz, mod_d) = (log(p[3] as f64, 1500.0, 15000.0), p[4] as f64 * 0.0016 * fs);
        let (shim_amt, mix) = (p[5] as f64, p[6] as f64 * 1.2);
        for i in 0..8 {
            self.damp[i].set_low_pass(fs, damp_hz);
            let len = BASE_MS[i] * self.scale * fs / 1000.0;
            self.gains[i] = 10f64.powf(-3.0 * len / (fs * t60));
        }
        let (feed_step, glide) = (1.0 / (fs * 0.02), 1.0 - (-1.0 / (fs * 0.25)).exp());
        for i in 0..8 {
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

            let mut x = 0.5 * (l[n] as f64 + r[n] as f64) * self.feed;
            self.pre.write(x);
            x = self.pre.read(pre_d) + self.shim_fb * shim_amt * 0.55;
            for k in 0..4 {
                // input diffusion (Schroeder allpasses)
                let d = self.diff[k].read(self.diff_len[k] as f64);
                let w = x + 0.62 * d;
                x = d - 0.62 * w;
                self.diff[k].write(w);
            }

            let len_scale = self.scale * fs / 1000.0;
            for i in 0..8 {
                let t = self.ms[i] * self.rc[i] + self.mc[i] * self.rs[i];
                self.mc[i] = self.mc[i] * self.rc[i] - self.ms[i] * self.rs[i];
                self.ms[i] = t;
                let d = BASE_MS[i] * len_scale + mod_d * (1.0 + self.ms[i]);
                let rd = self.lines[i].read(d);
                self.v[i] = self.damp[i].low_pass(rd) * self.gains[i];
            }
            let v = &mut self.v;
            let out_l = v[0] - v[2] + v[4] - v[6] + 0.5 * (v[1] + v[5]);
            let out_r = v[1] - v[3] + v[5] - v[7] + 0.5 * (v[2] + v[6]);

            // 8-point fast Walsh-Hadamard mix (energy preserving)
            let mut h = 1;
            while h < 8 {
                let mut i = 0;
                while i < 8 {
                    for j in i..i + h {
                        let (a, b) = (v[j], v[j + h]);
                        v[j] = a + b;
                        v[j + h] = a - b;
                    }
                    i += h << 1;
                }
                h <<= 1;
            }
            for i in 0..8 {
                let inj = x * if i & 1 == 0 { 0.5 } else { -0.5 };
                self.lines[i].write(v[i] * 0.353_553_39 + inj + 1e-20);
            }

            // shimmer loop with self-limiting so long decays can't build up without bound
            let ao = out_l.abs() + out_r.abs();
            self.shim_env += (if ao > self.shim_env { 0.001 } else { 0.00005 }) * (ao - self.shim_env);
            self.shim_fb = if shim_amt > 0.0 {
                let s = self.shim.process(0.5 * (out_l + out_r));
                self.shim_hp.high_pass(s).tanh() / (1.0 + 10.0 * self.shim_env)
            } else {
                0.0
            };

            let (wl, wr) = (self.wet_hp_l.high_pass(out_l) * 0.6, self.wet_hp_r.high_pass(out_r) * 0.6);
            let dry = 1.0 - 0.25 * mix * self.feed;
            l[n] = (l[n] as f64 * dry + wl * mix) as f32;
            r[n] = (r[n] as f64 * dry + wr * mix) as f32;
            peak = peak.max(wl.abs() + wr.abs());
        }
        self.quiet = if peak < 1e-5 { self.quiet + l.len() } else { 0 };
    }
}
