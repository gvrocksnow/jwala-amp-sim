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

/// Sidechain low cut: keeps the low strings from steering the whole gain.
const COMP_SC_HZ: f64 = 110.0;
/// The detector reports the highest peak of the last one to two windows of this length. A window
/// is longer than half a cycle of the low E, so the level of a held note is flat instead of
/// rippling at twice the note's pitch.
const COMP_HOLD_S: f64 = 0.007;
/// Above this the wet path gives back some of the top that gain reduction takes off the pick.
const COMP_AIR_HZ: f64 = 2200.0;

/// Smooth sustainer with parallel blend. The level is read from a held peak of the low-cut
/// sidechain and all the timing is done on the gain reduction in dB, so held notes are not
/// distorted by the gain wobbling within each cycle. Release is program dependent: a pick
/// transient lets go quickly, while a passage that has been held down recovers slowly, so notes
/// bloom without pumping. Highs are reduced a little less than the rest, keeping the sparkle.
#[derive(Default)]
pub struct Compressor {
    sc_hp: OnePole,
    air: OnePole,
    fs: f64,
    /// Highest sidechain peak in the window being filled, and in the one before it.
    peak: f64,
    peak_prev: f64,
    hold_left: u32,
    hold_len: u32,
    /// Gain reduction in dB: the quick follower, and the slow one that remembers held passages.
    g_fast: f64,
    g_slow: f64,
    atk: f64,
    rel_fast: f64,
    slow_atk: f64,
    slow_rel: f64,
    thr: f64,
    slope: f64,
    makeup: f64,
    blend: f64,
    /// Smoothed makeup and blend, so turning a knob does not zipper.
    s_makeup: f64,
    s_blend: f64,
    smooth: f64,
    primed: bool,
}

impl SampleFx for Compressor {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.sc_hp.set_low_pass(fs, COMP_SC_HZ);
        self.air.set_low_pass(fs, COMP_AIR_HZ);
        self.sc_hp.reset();
        self.air.reset();
        self.peak = 0.0;
        self.peak_prev = 0.0;
        self.hold_len = (fs * COMP_HOLD_S) as u32;
        self.hold_left = self.hold_len;
        self.g_fast = 0.0;
        self.g_slow = 0.0;
        self.rel_fast = coef(fs, 0.07);
        self.slow_atk = coef(fs, 0.25);
        self.slow_rel = coef(fs, 0.6);
        self.smooth = coef(fs, 0.02);
        self.primed = false;
    }
    fn update(&mut self, p: &[f32]) {
        let s = p[0] as f64;
        self.atk = coef(self.fs, log(p[1] as f64, 0.0008, 0.05));
        self.thr = -14.0 - 34.0 * s;
        let ratio = 2.0 + 10.0 * s;
        self.slope = 1.0 - 1.0 / ratio;
        self.makeup = db(-self.thr * self.slope * 0.55 + (-12.0 + 24.0 * p[2] as f64));
        self.blend = p[3] as f64;
        if !self.primed {
            self.primed = true;
            self.s_makeup = self.makeup;
            self.s_blend = self.blend;
        }
    }
    fn tick(&mut self, x: f64) -> f64 {
        // level: held peak of the low-cut sidechain
        let a = self.sc_hp.high_pass(x).abs();
        if self.hold_left == 0 {
            self.hold_left = self.hold_len;
            self.peak_prev = self.peak;
            self.peak = 0.0;
        }
        self.hold_left -= 1;
        self.peak = self.peak.max(a);
        let over = 20.0 * (self.peak.max(self.peak_prev) + 1e-9).log10() - self.thr;
        // soft knee (12 dB)
        let target = if over <= -6.0 {
            0.0
        } else if over >= 6.0 {
            over * self.slope
        } else {
            self.slope * (over + 6.0) * (over + 6.0) / 24.0
        };
        // timing, in dB
        self.g_fast += (if target > self.g_fast { self.atk } else { self.rel_fast }) * (target - self.g_fast);
        self.g_slow += (if self.g_fast > self.g_slow { self.slow_atk } else { self.slow_rel }) * (self.g_fast - self.g_slow);
        let gr = self.g_fast.max(0.7 * self.g_slow);
        let g = db(-gr);
        // the top is turned down a little less, by up to 4 dB
        let hi = self.air.high_pass(x);
        self.s_makeup += self.smooth * (self.makeup - self.s_makeup);
        self.s_blend += self.smooth * (self.blend - self.s_blend);
        let y = g * (x + (0.03 * gr).min(0.6) * hi) * self.s_makeup;
        x + (y - x) * self.s_blend
    }
}

#[cfg(test)]
mod compressor_tests {
    use super::*;

    const FS: f64 = 48000.0;
    const N: usize = 4800; // a tenth of a second: 10 Hz bins

    fn make(sustain: f32, attack: f32) -> Compressor {
        let mut c = Compressor::default();
        c.init(FS);
        c.update(&[sustain, attack, 0.5, 1.0, 0.0, 0.0, 0.0, 0.0]);
        c
    }

    /// Steady-state output for a sine, after a second of settling.
    fn render(c: &mut Compressor, hz: f64, amp: f64) -> Vec<f64> {
        let w = 2.0 * PI * hz / FS;
        (0..FS as usize + N).map(|i| c.tick(amp * (w * i as f64).sin())).skip(FS as usize).collect()
    }

    fn bin(x: &[f64], hz: f64) -> f64 {
        let w = 2.0 * PI * hz / FS;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            re += v * (w * i as f64).cos();
            im += v * (w * i as f64).sin();
        }
        (re * re + im * im) / (x.len() as f64 * x.len() as f64) * 4.0
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// A held low note comes out clean even at the fastest attack and heavy squeeze: the gain
    /// must not wobble inside each cycle.
    #[test]
    fn low_notes_stay_clean() {
        for hz in [80.0, 110.0, 220.0] {
            let x = render(&mut make(0.8, 0.0), hz, 0.3);
            let h1 = bin(&x, hz);
            let dist: f64 = (2..=7).map(|k| bin(&x, hz * k as f64)).sum();
            let thd = 10.0 * (dist / h1).max(1e-30).log10();
            println!("{hz:5.0} Hz  THD {thd:6.1} dB");
            assert!(thd < -55.0, "distortion on a {hz} Hz note: {thd:.1} dB");
        }
    }

    /// 30 dB of playing dynamics come out as a few dB.
    #[test]
    fn evens_out_dynamics() {
        let loud = rms(&render(&mut make(0.7, 0.35), 440.0, 0.5));
        let soft = rms(&render(&mut make(0.7, 0.35), 440.0, 0.5 * db(-30.0)));
        let range = 20.0 * (loud / soft).log10();
        println!("loud {loud:.3}  soft {soft:.3}  range {range:.1} dB");
        assert!((2.0..8.0).contains(&range), "{range:.1} dB left of 30");
    }

    /// A short hit lets go quickly; the same level held for seconds recovers slowly.
    #[test]
    fn release_follows_the_programme() {
        let w = 2.0 * PI * 440.0 / FS;
        let recovery = |hold_s: f64| {
            let mut c = make(0.7, 0.0);
            for i in 0..(FS * hold_s) as usize {
                c.tick(0.5 * (w * i as f64).sin());
            }
            for _ in 0..(FS * 0.15) as usize {
                c.tick(0.0);
            }
            c.g_fast.max(0.7 * c.g_slow)
        };
        let (short, long) = (recovery(0.03), recovery(3.0));
        println!("gain reduction 150 ms after: short hit {short:.1} dB, held {long:.1} dB");
        assert!(short < 7.0 && long > 2.0 * short, "short {short:.1} dB, held {long:.1} dB");
    }

    #[test]
    fn silence_stays_silent() {
        let mut c = make(1.0, 0.0);
        let peak = (0..20000).map(|_| c.tick(0.0).abs()).fold(0.0, f64::max);
        assert!(peak < 1e-9, "{peak} from silence");
    }
}

// Green screamer clipping stage: an op-amp with the diodes, a small cap and the DRIVE pot in its
// feedback loop, and a resistor + cap leg to ground that sets where the gain starts (720 Hz).
const OD_LEG_R: f64 = 4.7e3;
const OD_LEG_C: f64 = 47e-9;
const OD_FB_C: f64 = 51e-12;
/// Diode saturation current and slope voltage (1N914).
const OD_IS: f64 = 2.52e-9;
const OD_NVT: f64 = 1.752 * 25.85e-3;
/// The diode facing the other way is a softer one: it starts conducting earlier and gives way
/// more gradually, so the two halves of the wave are rounded differently and some second
/// harmonic stays in the sound.
const OD_IS_B: f64 = 12.0 * OD_IS;
const OD_NVT_B: f64 = 1.5 * OD_NVT;
// Tone stage: 1k + 0.22 uF low-pass (723 Hz), then an op-amp whose 20k pot steers a 220 ohm +
// 0.22 uF leg between cutting at its input and boosting in its feedback (1k).
const OD_TONE_R: f64 = 1e3;
const OD_TONE_C: f64 = 0.22e-6;
const OD_TONE_POT: f64 = 20e3;
const OD_TONE_RS: f64 = 220.0;

/// What the op-amp can swing: clean up to the knee, then rounding into the supply rail. Only a
/// hot boost in front of the pedal gets this far.
#[inline]
fn od_rail(u: f64) -> f64 {
    const KNEE: f64 = 3.0;
    const SPAN: f64 = 0.8;
    let a = u.abs();
    if a <= KNEE { u } else { (KNEE + SPAN * ((a - KNEE) / SPAN).tanh()).copysign(u) }
}

/// Green screamer: mid-hump soft clipping overdrive, modelled on the circuit. The clipping stage
/// is an op-amp with the diodes and the DRIVE pot in its feedback loop, solved every sample for
/// the voltage that carries the input current, with the clean signal riding on top: quiet
/// playing passes almost clean and the knee rounds over gradually as you dig in. The small cap
/// across the loop closes the top end down as DRIVE comes up. The solved curve is anti-aliased
/// the same way as `SoftClip` (averaged over the step from the previous sample). TONE is the
/// pedal's own active control, from the bare 723 Hz low-pass up to its full upper-mid lift.
#[derive(Default)]
pub struct Overdrive {
    inp: OnePole,
    leg: OnePole,
    cap: OnePole,
    out: OnePole,
    tone: Biquad,
    fs: f64,
    /// DRIVE resistance as a conductance.
    gr: f64,
    /// Last sample's feedback current, the voltage across the diodes that carries it, and the
    /// integral of that voltage over current up to there.
    i1: f64,
    v1: f64,
    f1: f64,
    lvl: f64,
}

impl SampleFx for Overdrive {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.inp.set_low_pass(fs, 20.0);
        self.leg.set_low_pass(fs, 1.0 / (2.0 * PI * OD_LEG_R * OD_LEG_C));
        self.out.set_low_pass(fs, 16.0);
        self.inp.reset();
        self.leg.reset();
        self.cap.reset();
        self.out.reset();
        self.tone.reset();
        (self.i1, self.v1, self.f1) = (0.0, 0.0, 0.0);
    }
    fn update(&mut self, p: &[f32]) {
        let d = p[0] as f64;
        self.gr = 1.0 / (51e3 + d * 500e3);
        self.cap.set_low_pass(self.fs, self.gr / (2.0 * PI * OD_FB_C));
        // TONE: the leg's series resistance (pot halves in parallel + 220 ohm) sweeps from the
        // pot's centre, where the leg does nothing, to the full-boost end
        let r = log((p[1] as f64).powf(0.75), OD_TONE_RS + OD_TONE_POT / 4.0, OD_TONE_RS);
        let k = 0.5 + 0.5 * (1.0 - 4.0 * (r - OD_TONE_RS) / OD_TONE_POT).max(0.0).sqrt();
        let (rc, c) = (OD_TONE_R * OD_TONE_C, OD_TONE_C);
        self.tone.analog(self.fs, [1.0, c * (r + k * OD_TONE_R), 0.0], [1.0, rc + c * r + OD_TONE_R * (1.0 - k) * c, rc * c * r]);
        self.lvl = db(-16.0 + 26.0 * p[2] as f64);
    }
    fn tick(&mut self, x: f64) -> f64 {
        let x = self.inp.high_pass(x);
        // current pulled through the leg to ground; all of it has to come round the feedback loop
        let i = self.cap.low_pass(self.leg.high_pass(x) / OD_LEG_R).clamp(-0.02, 0.02);
        // Newton for the voltage at which DRIVE resistor + diodes carry that current
        let mut v = self.v1;
        let (mut ep, mut en) = (0.0, 0.0);
        for _ in 0..24 {
            (ep, en) = (OD_IS * (v / OD_NVT).exp(), OD_IS_B * (-v / OD_NVT_B).exp());
            let f = self.gr * v + (ep - OD_IS) - (en - OD_IS_B) - i;
            let dv = (f / (self.gr + ep / OD_NVT + en / OD_NVT_B)).clamp(-0.1, 0.1);
            v -= dv;
            if dv.abs() < 1e-10 {
                break;
            }
        }
        // integral of v over i, in closed form from the solution: v i - (integral of i over v)
        let f = v * i - (0.5 * self.gr * v * v + OD_NVT * (ep - OD_IS) - OD_IS * v + OD_NVT_B * (en - OD_IS_B) + OD_IS_B * v);
        let di = i - self.i1;
        let vd = if di.abs() < 1e-10 { 0.5 * (v + self.v1) } else { (f - self.f1) / di };
        (self.i1, self.v1, self.f1) = (i, v, f);
        let y = self.out.high_pass(od_rail(x + vd));
        self.tone.process(y) * self.lvl
    }
}

#[cfg(test)]
mod overdrive_tests {
    use super::*;

    const FS: f64 = 192000.0;
    const N: usize = 19200; // 10 Hz bins

    fn make(drive: f32, tone: f32) -> Overdrive {
        let mut o = Overdrive::default();
        o.init(FS);
        o.update(&[drive, tone, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0]);
        o
    }

    fn render(fx: &mut dyn SampleFx, hz: f64, amp: f64) -> Vec<f64> {
        let w = 2.0 * PI * hz / FS;
        (0..FS as usize / 2 + N).map(|i| fx.tick(amp * (w * i as f64).sin())).skip(FS as usize / 2).collect()
    }

    fn bin(x: &[f64], hz: usize) -> f64 {
        let w = 2.0 * PI * hz as f64 / FS;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            re += v * (w * i as f64).cos();
            im += v * (w * i as f64).sin();
        }
        (re * re + im * im) / (x.len() as f64 * x.len() as f64) * 4.0
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn dbr(a: f64, b: f64) -> f64 {
        10.0 * (a / b).max(1e-30).log10()
    }

    /// Aliasing (what lands below 20 kHz without being a harmonic) stays far under the harmonics.
    #[test]
    fn clean_at_every_drive() {
        for drive in [0.0f32, 0.35, 0.7, 1.0] {
            let x = render(&mut make(drive, 1.0), 1240.0, 0.3);
            let (mut harm, mut other) = (0.0, 0.0);
            for hz in (20..20000).step_by(10) {
                let pw = bin(&x, hz);
                if hz % 1240 == 0 { harm += pw } else { other += pw }
            }
            let alias = dbr(other, harm);
            println!("DRIVE {drive:.2}  alias {alias:6.1} dB");
            assert!(alias < -80.0, "aliasing at DRIVE {drive}: {alias:.1} dB");
        }
    }

    /// Second harmonic stays in the sound however hard the pedal is driven, and turning DRIVE
    /// up from the default adds grit, not volume.
    #[test]
    fn warm_and_level_at_every_drive() {
        for amp in [0.15, 0.4] {
            let mut levels = vec![];
            for drive in [0.35f32, 0.7, 1.0] {
                let x = render(&mut make(drive, 0.5), 440.0, amp);
                let h2 = dbr(bin(&x, 880), bin(&x, 440));
                println!("amp {amp:.2} DRIVE {drive:.2}  rms {:.3}  H2 {h2:6.1} dB", rms(&x));
                assert!(h2 > -45.0, "no second harmonic at DRIVE {drive}: {h2:.1} dB");
                levels.push(rms(&x));
            }
            let spread = 20.0 * (levels[2] / levels[0]).log10();
            assert!(spread.abs() < 1.0, "level moves {spread:.1} dB with DRIVE");
        }
    }

    /// Soft playing comes through nearly clean; digging in brings the grit.
    #[test]
    fn cleans_up_when_played_softly() {
        let h3 = |amp: f64| {
            let x = render(&mut make(0.0, 0.5), 440.0, amp);
            dbr(bin(&x, 1320), bin(&x, 440))
        };
        let (soft, hard) = (h3(0.03), h3(0.4));
        println!("third harmonic: soft {soft:.1} dB, hard {hard:.1} dB");
        assert!(soft < -35.0 && hard > -25.0, "soft {soft:.1} dB, hard {hard:.1} dB");
    }

    /// TONE leaves the lows alone and lifts the upper mids steadily across its travel.
    #[test]
    fn tone_lifts_the_upper_mids() {
        let gain = |tone: f32, hz: f64| {
            let mut o = make(0.0, tone);
            o.gr = 1.0; // feedback shorted: the clipping stage is a clean unity buffer
            20.0 * (rms(&render(&mut o, hz, 0.001)) / o.lvl / 0.001 * 2f64.sqrt()).log10()
        };
        let at3k: Vec<f64> = [0.0f32, 0.25, 0.5, 0.75, 1.0].iter().map(|t| gain(*t, 3000.0)).collect();
        println!("3 kHz across TONE: {at3k:.1?} dB; 100 Hz: {:.1} / {:.1} dB", gain(0.0, 100.0), gain(1.0, 100.0));
        assert!(at3k.windows(2).all(|w| w[1] > w[0] + 1.5), "{at3k:?}");
        assert!(at3k[4] - at3k[0] > 9.0);
        assert!((gain(0.0, 100.0) - gain(1.0, 100.0)).abs() < 1.0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut o = make(1.0, 1.0);
        let peak = (0..20000).map(|_| o.tick(0.0).abs()).fold(0.0, f64::max);
        assert!(peak < 1e-9, "{peak} from silence");
    }
}

/// ln(cosh(u)), the antiderivative of tanh, written so it cannot overflow.
#[inline]
fn ln_cosh(u: f64) -> f64 {
    let a = u.abs();
    a + (-2.0 * a).exp().ln_1p() - std::f64::consts::LN_2
}

/// Soft clipper with first-order antiderivative anti-aliasing: the output is the clipping curve
/// averaged over the step from the previous sample to this one, which takes most of the aliasing
/// out of hard-driven edges. The curve is tanh on top and a lower, softer tanh underneath
/// (`floor` is how far the bottom half can swing; 1.0 makes it symmetric).
#[derive(Default)]
struct SoftClip {
    x1: f64,
    f1: f64,
}

impl SoftClip {
    #[inline]
    fn curve(u: f64, floor: f64) -> f64 {
        if u >= 0.0 { u.tanh() } else { floor * (u / floor).tanh() }
    }
    #[inline]
    fn integral(u: f64, floor: f64) -> f64 {
        if u >= 0.0 { ln_cosh(u) } else { floor * floor * ln_cosh(u / floor) }
    }
    /// Starts at rest with `x` at the input, so a biased stage does not open with a blip.
    fn reset(&mut self, x: f64, floor: f64) {
        self.x1 = x;
        self.f1 = Self::integral(x, floor);
    }
    #[inline]
    fn tick(&mut self, x: f64, floor: f64) -> f64 {
        let f = Self::integral(x, floor);
        let dx = x - self.x1;
        let y = if dx.abs() < 1e-5 { Self::curve(0.5 * (x + self.x1), floor) } else { (f - self.f1) / dx };
        self.x1 = x;
        self.f1 = f;
        y
    }
}

/// Resting bias on the distortion's first stage.
const DIST_BIAS: f64 = 0.2;

/// Two cascaded soft-clipping stages, valve fashion, with a sweepable filter. The lows are thinned
/// before the first stage (more so as DIST rises) so chords stay clear, and come back through a
/// gently driven parallel band so the sound stays thick. The first stage's bias follows the playing
/// level, which shifts the duty cycle and keeps second harmonic in the sound however hard it is
/// driven.
#[derive(Default)]
pub struct Distortion {
    hp: OnePole,
    low: OnePole,
    pre: OnePole,
    mid: OnePole,
    couple: OnePole,
    dc: OnePole,
    filt: OnePole,
    filt2: OnePole,
    c1: SoftClip,
    c2: SoftClip,
    fs: f64,
    env: f64,
    atk: f64,
    rel: f64,
    g1: f64,
    g2: f64,
    tight: f64,
    g_low: f64,
    lvl: f64,
}

impl SampleFx for Distortion {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.hp.set_low_pass(fs, 60.0);
        self.low.set_low_pass(fs, 220.0);
        self.pre.set_low_pass(fs, 9000.0);
        self.mid.set_low_pass(fs, 5500.0);
        self.couple.set_low_pass(fs, 90.0);
        self.dc.set_low_pass(fs, 20.0);
        for f in [&mut self.hp, &mut self.low, &mut self.pre, &mut self.mid, &mut self.couple, &mut self.dc, &mut self.filt, &mut self.filt2] {
            f.reset();
        }
        self.c1.reset(DIST_BIAS, 0.7);
        self.c2.reset(0.0, 0.85);
        self.env = 0.0;
        self.atk = coef(fs, 0.003);
        self.rel = coef(fs, 0.08);
    }
    fn update(&mut self, p: &[f32]) {
        let d = p[0] as f64;
        self.g1 = log(d, 3.0, 250.0);
        self.g2 = 2.0 + 4.0 * d;
        self.tight = 0.4 + 0.45 * d;
        self.g_low = 2.0 + 0.1 * self.g1;
        let fc = log(1.0 - p[1] as f64, 500.0, 16000.0);
        self.filt.set_low_pass(self.fs, fc);
        self.filt2.set_low_pass(self.fs, fc * 1.6);
        self.lvl = db(-26.0 + 26.0 * p[2] as f64);
    }
    fn tick(&mut self, x: f64) -> f64 {
        let hp = self.hp.high_pass(x);
        let low = self.low.low_pass(hp);
        let pre = self.pre.low_pass(hp - self.tight * low) * self.g1;
        let a = pre.abs();
        self.env += (if a > self.env { self.atk } else { self.rel }) * (a - self.env);
        // first stage: uneven halves, bias riding on the envelope
        let s1 = self.c1.tick(pre + DIST_BIAS + 0.3 * self.env, 0.7) - DIST_BIAS.tanh();
        let c = self.couple.high_pass(self.mid.low_pass(s1));
        // second stage, leaning the other way
        let s2 = self.dc.high_pass(self.c2.tick(-c * self.g2, 0.85));
        let v = -0.44 * s2 + 0.22 * (low * self.g_low).tanh();
        let f1 = self.filt.low_pass(v);
        self.filt2.low_pass(f1) * self.lvl
    }
}

#[cfg(test)]
mod distortion_tests {
    use super::*;

    const FS: f64 = 192000.0; // the oversampled rate the pedal runs at in a 48 kHz session
    const F0: usize = 1240;
    const N: usize = 19200; // a tenth of a second: 10 Hz bins, F0 lands exactly on one

    fn knobs(dist: f32) -> [f32; 8] {
        [dist, 0.45, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0]
    }

    /// Steady-state output for a sine of amplitude 0.15 at F0, after a second of settling.
    fn render(dist: f32) -> Vec<f64> {
        let mut d = Distortion::default();
        d.init(FS);
        d.update(&knobs(dist));
        let w = 2.0 * PI * F0 as f64 / FS;
        (0..FS as usize + N).map(|i| d.tick(0.15 * (w * i as f64).sin())).skip(FS as usize).collect()
    }

    /// Power in the bin at `hz` (a multiple of 10).
    fn bin(x: &[f64], hz: usize) -> f64 {
        let w = 2.0 * PI * hz as f64 / FS;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            re += v * (w * i as f64).cos();
            im += v * (w * i as f64).sin();
        }
        (re * re + im * im) / (x.len() as f64 * x.len() as f64) * 4.0
    }

    /// Second harmonic stays in the sound at every drive setting, and what lands below 20 kHz
    /// without being a harmonic (aliasing) stays far under the harmonics.
    #[test]
    fn rich_and_clean_at_every_drive() {
        for dist in [0.25f32, 0.5, 0.75, 1.0] {
            let x = render(dist);
            let (mut harm, mut other) = (0.0, 0.0);
            for hz in (20..20000).step_by(10) {
                let pw = bin(&x, hz);
                if hz % F0 == 0 { harm += pw } else { other += pw }
            }
            let dbr = |a: f64, b: f64| 10.0 * (a / b).max(1e-30).log10();
            let (h2, alias) = (dbr(bin(&x, 2 * F0), bin(&x, F0)), dbr(other, harm));
            let rms = (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
            println!("DIST {dist:.2}  rms {rms:.3}  H2 {h2:6.1} dB  alias {alias:6.1} dB");
            assert!(h2 > -30.0, "no second harmonic at DIST {dist}: {h2:.1} dB");
            assert!(alias < -80.0, "aliasing at DIST {dist}: {alias:.1} dB");
            assert!((0.05..0.12).contains(&rms), "level at DIST {dist}: {rms}");
        }
    }

    /// Silence in gives silence out: the biased first stage must not open with a blip.
    #[test]
    fn silence_stays_silent() {
        let mut d = Distortion::default();
        d.init(FS);
        d.update(&knobs(1.0));
        let peak = (0..20000).map(|_| d.tick(0.0).abs()).fold(0.0, f64::max);
        assert!(peak < 1e-9, "{peak} from silence");
    }
}

// Muffler clipping stage: a transistor gain stage fed through a coupling cap and a resistor, with
// a 470k resistor, a small cap and a pair of diodes from its output back to its input.
const FUZZ_IN_R: f64 = 10e3;
const FUZZ_IN_C: f64 = 0.1e-6;
const FUZZ_FB_R: f64 = 470e3;
const FUZZ_FB_C: f64 = 470e-12;
/// The two diodes of a pair are never quite alike: the second conducts a little earlier and
/// gives way a little more gradually, so the halves of the wave differ and the fuzz keeps some
/// even harmonics.
const FUZZ_IS_B: f64 = 2.5 * OD_IS;
const FUZZ_NVT_B: f64 = 1.12 * OD_NVT;
/// Gain of the booster in front of the SUSTAIN pot.
const FUZZ_BOOST: f64 = 6.0;
/// Once the diodes conduct, the feedback cap only rounds the edges of the wave.
const FUZZ_EDGE_HZ: f64 = 7000.0;
/// How far the second stage is pushed off centre, as a fraction of the level it is being fed.
const FUZZ_LEAN: f64 = 0.05;
// Tone stack: a 39k + 10 nF low-pass and a 4 nF + 22k high-pass, with the 100k TONE pot strung
// between their outputs.
const FUZZ_TONE_R1: f64 = 39e3;
const FUZZ_TONE_C1: f64 = 10e-9;
const FUZZ_TONE_C2: f64 = 4e-9;
const FUZZ_TONE_R2: f64 = 22e3;
const FUZZ_TONE_POT: f64 = 100e3;
/// Recovery gain after the tone stack: with VOLUME at noon the fuzz sits level with the drives.
const FUZZ_MAKEUP: f64 = 6.4;

/// One clipping stage of the fuzz: returns the voltage across the feedback loop (resistor and
/// diode pair) that carries the input current `i`, anti-aliased by averaging the solved curve
/// over the step from the previous sample, as `Overdrive` does.
#[derive(Default)]
struct FuzzClip {
    i1: f64,
    v1: f64,
    f1: f64,
}

impl FuzzClip {
    fn reset(&mut self) {
        (self.i1, self.v1, self.f1) = (0.0, 0.0, 0.0);
    }
    #[inline]
    fn tick(&mut self, i: f64) -> f64 {
        const GR: f64 = 1.0 / FUZZ_FB_R;
        let i = i.clamp(-5e-3, 5e-3);
        let mut v = self.v1;
        let (mut ep, mut en) = (0.0, 0.0);
        for _ in 0..24 {
            (ep, en) = (OD_IS * (v / OD_NVT).exp(), FUZZ_IS_B * (-v / FUZZ_NVT_B).exp());
            let f = GR * v + (ep - OD_IS) - (en - FUZZ_IS_B) - i;
            let dv = (f / (GR + ep / OD_NVT + en / FUZZ_NVT_B)).clamp(-0.1, 0.1);
            v -= dv;
            if dv.abs() < 1e-10 {
                break;
            }
        }
        // integral of v over i, in closed form from the solution: v i - (integral of i over v)
        let f = v * i - (0.5 * GR * v * v + OD_NVT * (ep - OD_IS) - OD_IS * v + FUZZ_NVT_B * (en - FUZZ_IS_B) + FUZZ_IS_B * v);
        let di = i - self.i1;
        let vd = if di.abs() < 1e-11 { 0.5 * (v + self.v1) } else { (f - self.f1) / di };
        (self.i1, self.v1, self.f1) = (i, v, f);
        vd
    }
}

/// Big wall-of-fuzz, modelled on the circuit: a booster, the SUSTAIN pot, two clipping stages
/// and the scooped tone stack. Each clipping stage is a gain stage with a diode pair in its
/// feedback loop, solved every sample for the voltage that carries the input current, so the
/// knee is the diodes' own gradual one and notes sink into the fuzz smoothly as they die away.
/// The feedback cap makes each stage dark until the diodes conduct, which is what keeps the
/// sustain thick and singing instead of fizzy; the coupling caps between the stages keep chords
/// from turning to mud. The second stage leans off centre in proportion to the level it is fed,
/// which shifts the duty cycle of the wave and keeps second harmonic in the sound from the pick
/// to the tail of the note. TONE is the pedal's passive stack: lows only at one end, highs only at
/// the other, the mids scooped in between.
#[derive(Default)]
pub struct Fuzz {
    inp: OnePole,
    boost: OnePole,
    hp1: OnePole,
    hp2: OnePole,
    cap1: OnePole,
    cap2: OnePole,
    edge1: OnePole,
    edge2: OnePole,
    out: OnePole,
    c1: FuzzClip,
    c2: FuzzClip,
    tone: Biquad,
    fs: f64,
    env: f64,
    atk: f64,
    rel: f64,
    sustain: f64,
    lvl: f64,
}

impl SampleFx for Fuzz {
    fn init(&mut self, fs: f64) {
        self.fs = fs;
        self.inp.set_low_pass(fs, 30.0);
        self.boost.set_low_pass(fs, 3400.0);
        let couple = 1.0 / (2.0 * PI * FUZZ_IN_R * FUZZ_IN_C);
        self.hp1.set_low_pass(fs, couple);
        self.hp2.set_low_pass(fs, couple);
        let cap = 1.0 / (2.0 * PI * FUZZ_FB_R * FUZZ_FB_C);
        self.cap1.set_low_pass(fs, cap);
        self.cap2.set_low_pass(fs, cap);
        self.edge1.set_low_pass(fs, FUZZ_EDGE_HZ);
        self.edge2.set_low_pass(fs, FUZZ_EDGE_HZ);
        self.out.set_low_pass(fs, 20.0);
        for f in [&mut self.inp, &mut self.boost, &mut self.hp1, &mut self.hp2, &mut self.cap1, &mut self.cap2, &mut self.edge1, &mut self.edge2, &mut self.out] {
            f.reset();
        }
        self.c1.reset();
        self.c2.reset();
        self.tone.reset();
        self.env = 0.0;
        self.atk = coef(fs, 0.003);
        self.rel = coef(fs, 0.08);
    }
    fn update(&mut self, p: &[f32]) {
        self.sustain = FUZZ_BOOST * log(p[0] as f64, 0.01, 1.0);
        // TONE: the output is taken from the pot's wiper, t of the way from the low-pass end to
        // the high-pass end
        let t = p[1] as f64;
        let (g1, g2, gp) = (1.0 / FUZZ_TONE_R1, 1.0 / FUZZ_TONE_R2, 1.0 / FUZZ_TONE_POT);
        let (c1, c2) = (FUZZ_TONE_C1, FUZZ_TONE_C2);
        self.tone.analog(
            self.fs,
            [g1 * (g2 + gp) - t * g1 * g2, c2 * (g1 + gp), t * c1 * c2],
            [(g1 + gp) * (g2 + gp) - gp * gp, c1 * (g2 + gp) + c2 * (g1 + gp), c1 * c2],
        );
        self.lvl = db(-26.0 + 24.0 * p[2] as f64) * FUZZ_MAKEUP;
    }
    fn tick(&mut self, x: f64) -> f64 {
        let x = self.boost.low_pass(self.inp.high_pass(x)) * self.sustain;
        // each stage inverts
        let i = self.cap1.low_pass(self.hp1.high_pass(x) / FUZZ_IN_R);
        let s1 = -self.edge1.low_pass(self.c1.tick(i));
        let i = self.cap2.low_pass(self.hp2.high_pass(s1) / FUZZ_IN_R);
        let a = i.abs();
        self.env += (if a > self.env { self.atk } else { self.rel }) * (a - self.env);
        // the second pair is in the other way round
        let s2 = self.edge2.low_pass(self.c2.tick(-i - FUZZ_LEAN * self.env));
        self.tone.process(self.out.high_pass(s2)) * self.lvl
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    const FS: f64 = 192000.0;
    const N: usize = 19200; // 10 Hz bins

    fn make(sustain: f32, tone: f32) -> Fuzz {
        let mut f = Fuzz::default();
        f.init(FS);
        f.update(&[sustain, tone, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0]);
        f
    }

    fn render(fx: &mut dyn SampleFx, hz: f64, amp: f64) -> Vec<f64> {
        let w = 2.0 * PI * hz / FS;
        (0..FS as usize / 2 + N).map(|i| fx.tick(amp * (w * i as f64).sin())).skip(FS as usize / 2).collect()
    }

    fn bin(x: &[f64], hz: usize) -> f64 {
        let w = 2.0 * PI * hz as f64 / FS;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            re += v * (w * i as f64).cos();
            im += v * (w * i as f64).sin();
        }
        (re * re + im * im) / (x.len() as f64 * x.len() as f64) * 4.0
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn dbr(a: f64, b: f64) -> f64 {
        10.0 * (a / b).max(1e-30).log10()
    }

    /// Aliasing (what lands below 20 kHz without being a harmonic) stays far under the harmonics.
    #[test]
    fn clean_at_every_sustain() {
        for sustain in [0.0f32, 0.3, 0.6, 1.0] {
            for tone in [0.5f32, 1.0] {
                let x = render(&mut make(sustain, tone), 1240.0, 0.15);
                let (mut harm, mut other) = (0.0, 0.0);
                for hz in (20..20000).step_by(10) {
                    let pw = bin(&x, hz);
                    if hz % 1240 == 0 { harm += pw } else { other += pw }
                }
                let alias = dbr(other, harm);
                println!("SUSTAIN {sustain:.2} TONE {tone:.2}  alias {alias:6.1} dB");
                assert!(alias < -78.0, "aliasing at SUSTAIN {sustain}, TONE {tone}: {alias:.1} dB");
            }
        }
    }

    /// Second harmonic stays in the sound at every setting, for low and high notes alike.
    #[test]
    fn keeps_even_harmonics() {
        for sustain in [0.0f32, 0.6, 1.0] {
            for hz in [110usize, 1240] {
                let x = render(&mut make(sustain, 0.5), hz as f64, 0.15);
                let h2 = dbr(bin(&x, 2 * hz), bin(&x, hz));
                println!("SUSTAIN {sustain:.2} {hz} Hz  H2 {h2:6.1} dB");
                assert!(h2 > -32.0, "no second harmonic at SUSTAIN {sustain}, {hz} Hz: {h2:.1} dB");
            }
        }
    }

    /// A note that has died away to a fraction of its level is still nearly as loud coming out,
    /// and SUSTAIN adds length, not volume.
    #[test]
    fn sustains_at_an_even_level() {
        let level = |sustain: f32, amp: f64| rms(&render(&mut make(sustain, 0.5), 440.0, amp));
        let (loud, tail) = (level(0.6, 0.15), level(0.6, 0.02));
        let spread = 20.0 * (level(1.0, 0.15) / level(0.3, 0.15)).log10();
        println!("loud {loud:.3}, tail {tail:.3}, SUSTAIN 0.3 to 1.0 moves the level {spread:.1} dB");
        assert!((0.13..0.2).contains(&loud), "level {loud}");
        assert!(20.0 * (loud / tail).log10() < 1.5);
        assert!(spread.abs() < 1.0);
    }

    /// TONE goes from lows only to highs only, with the mids scooped at noon.
    #[test]
    fn tone_scoops_the_mids() {
        let gain = |tone: f32, hz: f64| make(0.6, tone).tone.magnitude_db(FS, hz);
        for tone in [0.0f32, 0.5, 1.0] {
            println!("TONE {tone:.1}: 100 Hz {:5.1}  1 kHz {:5.1}  5 kHz {:5.1} dB", gain(tone, 100.0), gain(tone, 1000.0), gain(tone, 5000.0));
        }
        assert!(gain(0.5, 1000.0) < gain(0.5, 100.0) - 3.0 && gain(0.5, 1000.0) < gain(0.5, 5000.0) - 3.0);
        assert!(gain(0.0, 5000.0) < gain(0.0, 100.0) - 12.0);
        assert!(gain(1.0, 100.0) < gain(1.0, 5000.0) - 12.0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = make(1.0, 1.0);
        let peak = (0..20000).map(|_| f.tick(0.0).abs()).fold(0.0, f64::max);
        assert!(peak < 1e-9, "{peak} from silence");
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

/// One synthetic take for the doubler: a micro-pitch-shifted, retimed, decorrelated copy.
///
/// The read point sits `lo + u * span` samples behind the input. A constant detune makes `u`
/// creep steadily; when it runs out of window it splices (short crossfade) to the far end at the
/// spot where the waveform lines up best, so the splice is inaudible. Each picked note also
/// splices the take to a fresh random position, which is what makes it early or late per note
/// the way a second performance would be.
struct Take {
    /// +1 plays sharp, -1 flat
    sign: f64,
    u: f64,
    old_u: f64,
    /// splice crossfade progress, 1 = no splice running
    xf: f64,
    xf_step: f64,
    drift: f64,
    drift_mid: f64,
    drift_target: f64,
    gain: f64,
    gain_target: f64,
    tilt: f64,
    tilt_target: f64,
    timer: f64,
    split: OnePole,
    ap: [DelayLine; 3],
    ap_len: [usize; 3],
    rng: Rng,
}

impl Take {
    fn new(seed: u64, sign: f64, u: f64) -> Self {
        Take {
            sign,
            u,
            old_u: u,
            xf: 1.0,
            xf_step: 0.0,
            drift: 0.0,
            drift_mid: 0.0,
            drift_target: 0.0,
            gain: 1.0,
            gain_target: 1.0,
            tilt: 0.0,
            tilt_target: 0.0,
            timer: 0.0,
            split: OnePole::default(),
            ap: [DelayLine::new(1), DelayLine::new(1), DelayLine::new(1)],
            ap_len: [1; 3],
            rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        }
    }

    fn prepare(&mut self, fs: f64, ap_ms: [f64; 3]) {
        for k in 0..3 {
            self.ap_len[k] = ((ap_ms[k] * fs / 1000.0) as usize).max(1);
            self.ap[k] = DelayLine::new(self.ap_len[k] + 1);
        }
        self.split.set_low_pass(fs, 800.0);
    }

    /// Splice to position `u` over `1 / step` samples (ignored while a splice is still running).
    fn jump(&mut self, u: f64, step: f64) {
        if self.xf >= 1.0 {
            (self.old_u, self.u, self.xf, self.xf_step) = (self.u, u, 0.0, step);
        }
    }

    fn tick(&mut self, dl: &DelayLine, fs: f64, lo: f64, span: f64, amt: f64, slew: f64) -> f64 {
        self.timer -= 1.0;
        if self.timer <= 0.0 {
            // a new "intention" every 120-380 ms: tuning, picking strength and pick position wander
            self.drift_target = self.rng.next_f64() * 2.0 - 1.0;
            self.gain_target = 1.0 + (self.rng.next_f64() * 2.0 - 1.0) * 0.12 * amt;
            self.tilt_target = (self.rng.next_f64() * 2.0 - 1.0) * 0.2 * amt;
            self.timer = fs * (0.12 + 0.26 * self.rng.next_f64());
        }
        // two cascaded smoothers: the detune glides, never steps
        self.drift_mid += slew * (self.drift_target - self.drift_mid);
        self.drift += slew * (self.drift_mid - self.drift);
        self.gain += slew * (self.gain_target - self.gain);
        self.tilt += slew * (self.tilt_target - self.tilt);

        // AMOUNT 50% = 7 cents either side of the original, wandering by another +-1.5
        let cents = amt * (14.0 * self.sign + 3.0 * self.drift);
        let du = -cents * 5.7762e-4 / span; // playing sharp = the delay shrinks
        self.u += du;
        if self.xf < 1.0 {
            self.old_u += du;
            self.xf = (self.xf + self.xf_step).min(1.0);
            self.u = self.u.clamp(0.0, 1.0);
        } else if self.u < 0.0 || self.u > 1.0 {
            // out of window: restart at the other end, in phase with what is playing now
            let search = (0.5 * span).min(0.012 * fs);
            let from = if self.u < 0.0 { lo + span - search } else { lo };
            let to = align(dl, lo + self.u * span, from, search as usize, (0.0025 * fs) as usize);
            self.jump(((to - lo) / span).clamp(0.0, 1.0), 1.0 / (0.012 * fs));
        }

        let mut y = dl.read(lo + self.u * span);
        if self.xf < 1.0 {
            let g = 0.5 - 0.5 * (PI * self.xf).cos();
            y = y * g + dl.read(lo + self.old_u * span) * (1.0 - g);
        }
        // a short allpass chain (different per take) scrambles the phase, so the takes stop being
        // copies of each other and their mono sum does not comb
        for k in 0..3 {
            y = allpass(&mut self.ap[k], self.ap_len[k], 0.5, y);
        }
        let low = self.split.low_pass(y);
        (low * (1.0 - self.tilt) + (y - low) * (1.0 + self.tilt)) * self.gain
    }
}

/// The delay in `from..=from + search` whose surrounding `2 * half` samples best match those
/// around delay `cur` (normalized correlation).
fn align(dl: &DelayLine, cur: f64, from: f64, search: usize, half: usize) -> f64 {
    let (c0, f0) = (cur.max(half as f64) as usize, from.max(half as f64) as usize);
    let score = |off: usize| {
        let (mut c, mut e) = (0.0, 1e-12);
        let mut m = 0;
        while m < 2 * half {
            let a = dl.tap(f0 + off + half - m);
            c += a * dl.tap(c0 + half - m);
            e += a * a;
            m += 2;
        }
        c / e.sqrt()
    };
    let (mut best, mut best_off) = (f64::MIN, 0);
    for off in (0..=search).step_by(3) {
        let sc = score(off);
        if sc > best {
            (best, best_off) = (sc, off);
        }
    }
    for off in best_off.saturating_sub(2)..=(best_off + 2).min(search) {
        let sc = score(off);
        if sc > best {
            (best, best_off) = (sc, off);
        }
    }
    (f0 + best_off) as f64
}

/// Stereo doubler: fakes a double-tracked part. Two synthetic takes of the signal, one a few
/// cents sharp and one a few cents flat, are retimed note by note inside the same DELAY window
/// (so neither side leads on average and the image stays centred), given their own wandering
/// level and tone, phase-scrambled, then spread left/right.
pub struct Doubler {
    dl: DelayLine,
    a: Take,
    b: Take,
    rng: Rng,
    span: f64,
    env: f64,
    env_slow: f64,
    hold: f64,
    fs: f64,
    fade: f64,
    fade_step: f64,
}

impl Doubler {
    fn new() -> Self {
        Doubler { dl: DelayLine::new(1), a: Take::new(11, 1.0, 0.3), b: Take::new(23, -1.0, 0.7), rng: Rng(0x5DEE_CE66_D1CE_4E5B), span: 1.0, env: 0.0, env_slow: 0.0, hold: 0.0, fs: 48000.0, fade: 0.0, fade_step: 0.0 }
    }
}

impl StereoFx for Doubler {
    fn prepare(&mut self, fs: f64, enabled: bool, p: &[f32]) {
        self.fs = fs;
        self.dl = DelayLine::new((fs * 0.07) as usize);
        self.a.prepare(fs, [0.61, 1.37, 2.29]);
        self.b.prepare(fs, [0.77, 1.63, 2.63]);
        self.span = doubler_delay_ms(p[1]) * fs / 1000.0;
        self.fade_step = 1.0 / (fs * 0.02);
        self.fade = if enabled { 1.0 } else { 0.0 };
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], p: &[f32], enabled: bool) {
        if !enabled && self.fade == 0.0 {
            return;
        }
        let fs = self.fs;
        let (amt, span_to, w, mix) = (p[0] as f64, doubler_delay_ms(p[1]) * fs / 1000.0, p[2] as f64, p[3] as f64);
        // the takes never get closer than 4 ms to the input: a note is spotted on the way in and
        // both takes have moved to their new timing before it reaches them
        let lo = 0.004 * fs;
        let (slew, span_slew) = (coef(fs, 0.22), coef(fs, 0.08));
        let (env_rel, env_slow) = (1.0 - coef(fs, 0.008), coef(fs, 0.04));
        let spread = amt.sqrt();
        let jump_step = 1.0 / (0.003 * fs);
        let t = if enabled { 1.0 } else { 0.0 };
        for i in 0..l.len() {
            self.fade = ramp(self.fade, t, self.fade_step);
            self.span += span_slew * (span_to - self.span);
            let mono = 0.5 * (l[i] as f64 + r[i] as f64);
            self.dl.write(mono);

            // pick attack: the level jumps well clear of its recent average
            self.env = mono.abs().max(self.env * env_rel);
            self.env_slow += env_slow * (self.env - self.env_slow);
            self.hold -= 1.0;
            if self.hold <= 0.0 && self.env > 1.8 * self.env_slow + 1e-3 {
                self.hold = 0.07 * fs;
                self.a.jump(0.5 + (self.rng.next_f64() - 0.5) * spread, jump_step);
                self.b.jump(0.5 + (self.rng.next_f64() - 0.5) * spread, jump_step);
            }

            let ta = self.a.tick(&self.dl, fs, lo, self.span, amt, slew);
            let tb = self.b.tick(&self.dl, fs, lo, self.span, amt, slew);
            let mid = 0.5 * (ta + tb);
            let (wl, wr) = (mid + w * (ta - mid), mid + w * (tb - mid));
            let m = mix * self.fade;
            l[i] = (l[i] as f64 * (1.0 - m) + wl * m) as f32;
            r[i] = (r[i] as f64 * (1.0 - m) + wr * m) as f32;
        }
    }
}

#[cfg(test)]
mod doubler_tests {
    use super::*;

    const FS: f64 = 48000.0;

    fn run(p: [f32; 4], seconds: f64, input: impl Fn(usize) -> f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let mut d = Doubler::new();
        d.prepare(FS, true, &p);
        let n = (FS * seconds) as usize;
        let dry: Vec<f32> = (0..n).map(&input).collect();
        let (mut l, mut r) = (dry.clone(), dry.clone());
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            d.process(a, b, &p, true);
        }
        (dry, l, r)
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// A new plucked note (a few decaying harmonics) every 230 ms, walking over a scale.
    fn plucks(i: usize) -> f32 {
        let period = (0.23 * FS) as usize;
        let (note, k) = (i / period, (i % period) as f64 / FS);
        let f = 110.0 * 2f64.powf([0.0, 3.0, 5.0, 7.0, 10.0, 12.0, 15.0][note % 7] / 12.0);
        let mut y = 0.0;
        for h in 1..=6 {
            y += (2.0 * PI * f * h as f64 * k).sin() / h as f64 * (-k * (3.0 + 2.0 * h as f64)).exp();
        }
        (0.3 * y) as f32
    }

    fn up_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    /// Loudness matches the dry signal, the sides are balanced, the mono sum keeps its body.
    #[test]
    fn level_balance_and_mono() {
        let (dry, l, r) = run([0.5, 0.35, 1.0, 1.0], 8.0, plucks);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        let (d, a, b) = (rms(&dry), rms(&l), rms(&r));
        let mono: Vec<f32> = l.iter().zip(&r).map(|(x, y)| 0.5 * (x + y)).collect();
        println!("dry {d:.4} l {a:.4} r {b:.4} mono {:.4}", rms(&mono));
        assert!((20.0 * (a / d).log10()).abs() < 1.5 && (20.0 * (b / d).log10()).abs() < 1.5, "level {d} -> {a} / {b}");
        assert!((20.0 * (a / b).log10()).abs() < 0.8, "unbalanced: {a} vs {b}");
        assert!(rms(&mono) > 0.6 * d, "mono sum is hollow: {} of {d}", rms(&mono));
    }

    /// One take plays steadily sharp and the other steadily flat, splices included.
    #[test]
    fn takes_are_detuned_either_way() {
        let (_, l, r) = run([0.5, 0.35, 1.0, 1.0], 9.0, |i| (0.3 * (2.0 * PI * 1000.0 * i as f64 / FS).sin()) as f32);
        let (from, secs) = (FS as usize, 8.0);
        let (fl, fr) = (up_crossings(&l[from..]) as f64 / secs, up_crossings(&r[from..]) as f64 / secs);
        println!("left {fl} Hz, right {fr} Hz");
        // 7 +- 1.5 cents at 1 kHz = 3.2 to 4.9 Hz
        assert!(fl > 1002.5 && fl < 1006.0, "left at {fl} Hz");
        assert!(fr < 997.5 && fr > 994.0, "right at {fr} Hz");
    }

    /// The splice at the end of the window lands in phase: a steady tone stays steady.
    #[test]
    fn splices_are_clean() {
        let (_, l, _) = run([1.0, 0.0, 1.0, 1.0], 6.0, |i| (0.3 * (2.0 * PI * 440.0 * i as f64 / FS).sin()) as f32);
        // AMOUNT 100% on a 6 ms window splices about every 0.75 s
        let levels: Vec<f64> = l[FS as usize..].chunks_exact(480).map(rms).collect();
        let (lo, hi) = levels.iter().fold((f64::MAX, 0.0f64), |(a, b), v| (a.min(*v), b.max(*v)));
        println!("splice level {lo} .. {hi}");
        assert!(lo > 0.7 * hi, "level dips at a splice: {lo} vs {hi}");
    }

    /// Each note lands at its own time inside the DELAY window, and neither side leads on average.
    #[test]
    fn notes_are_retimed() {
        let gap = (0.3 * FS) as usize;
        let p = [0.5, 0.35, 1.0, 1.0];
        let (_, l, r) = run(p, 12.0, |i| if i % gap == 0 { 0.5 } else { 0.0 });
        let arrival = |x: &[f32]| -> Vec<f64> {
            x.chunks_exact(gap).map(|c| (0..gap).max_by(|&a, &b| c[a].abs().total_cmp(&c[b].abs())).unwrap() as f64 * 1000.0 / FS).collect()
        };
        let (al, ar) = (arrival(&l), arrival(&r));
        println!("left {al:.1?}\nright {ar:.1?}");
        let window = 4.0 + doubler_delay_ms(p[1]) + 3.0; // + the allpass chain
        for v in al.iter().chain(&ar) {
            assert!(*v >= 4.0 && *v <= window, "note arrived at {v} ms");
        }
        let range = |v: &[f64]| v.iter().fold(0.0f64, |m, x| m.max(*x)) - v.iter().fold(f64::MAX, |m, x| m.min(*x));
        assert!(range(&al) > 2.0 && range(&ar) > 2.0, "timing does not vary: {al:?}");
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        assert!((mean(&al) - mean(&ar)).abs() < 2.5, "one side leads: {} vs {} ms", mean(&al), mean(&ar));
        let left_first = al.iter().zip(&ar).filter(|(a, b)| a < b).count();
        assert!(left_first > al.len() / 5 && left_first < al.len() * 4 / 5, "left first {left_first} of {}", al.len());
    }

    /// The two sides are different signals at full WIDTH and the same signal at none.
    #[test]
    fn width_decorrelates() {
        let corr = |l: &[f32], r: &[f32]| l.iter().zip(r).map(|(a, b)| (*a * *b) as f64).sum::<f64>() / (rms(l) * rms(r) * l.len() as f64);
        let (_, l, r) = run([0.5, 0.35, 1.0, 1.0], 8.0, plucks);
        println!("correlation {}", corr(&l, &r));
        assert!(corr(&l, &r).abs() < 0.5, "sides correlate: {}", corr(&l, &r));
        let (_, l, r) = run([0.5, 0.35, 0.0, 1.0], 2.0, plucks);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn bypassed_is_untouched() {
        let p = [0.5, 0.35, 1.0, 1.0];
        let mut d = Doubler::new();
        d.prepare(FS, false, &p);
        let dry: Vec<f32> = (0..4800).map(plucks).collect();
        let (mut l, mut r) = (dry.clone(), dry.clone());
        d.process(&mut l, &mut r, &p, false);
        assert!(l == dry && r == dry);
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
