//! The LEAD II preamp: five cascaded 12AX7 stages modelled at circuit level, in real volts.
//!
//! Each stage is a common-cathode triode (Koren plate-current model) with its plate load,
//! cathode resistor and bypass cap, and an input network of coupling cap, attenuator, grid
//! stopper and Miller capacitance. Grid current charges the coupling cap and plate current
//! charges the cathode cap, so blocking distortion, bias shift and recovery after a hard
//! pick come from the capacitor states rather than from an envelope follower.
//!
//! The component values are those of the SLO-family overdrive preamp that the 5150's lead
//! channel descends from (partially bypassed 1.8k cathodes, a 39k cold clipper as the third
//! stage, large grid stoppers). They are a starting point for voicing, not a transcription
//! of one schematic.
//!
//! Per sample each stage solves one scalar equation for the grid-to-cathode voltage. The
//! plate current along the stage's load line and the grid current are both functions of
//! that voltage alone, so they are tabulated once and the solve is a few table lookups.

use super::filters::{Biquad, OnePole};

/// Grid-to-cathode range of the tables. Below it the valve is cut off (constant); above it
/// the tables extrapolate linearly (plate bottomed, grid conducting through its resistance).
const X_MIN: f64 = -12.0;
const X_MAX: f64 = 6.0;
const TAB_N: usize = 4096;
const X_STEP: f64 = (X_MAX - X_MIN) / TAB_N as f64;

/// Peak DI sample level -> volts at the input jack (a hot humbucker is around 1 V peak).
pub const INPUT_VOLTS: f64 = 3.0;
/// Plate swing of the last stage (volts) -> the level the tone stack and power amp expect.
const OUT_SCALE: f64 = 1.0 / 150.0;

/// Koren's 12AX7 plate current in amps.
fn koren(vpk: f64, vgk: f64) -> f64 {
    const MU: f64 = 100.0;
    const EX: f64 = 1.4;
    const KG1: f64 = 1060.0;
    const KP: f64 = 600.0;
    const KVB: f64 = 300.0;
    if vpk <= 0.0 {
        return 0.0;
    }
    let a = KP * (1.0 / MU + vgk / (KVB + vpk * vpk).sqrt());
    let sp = if a > 30.0 { a } else { a.exp().ln_1p() };
    2.0 * (vpk / KP * sp).powf(EX) / KG1
}

/// Grid current in amps: a soft-kneed diode that conducts as the grid goes positive.
fn grid_current(vgk: f64) -> f64 {
    const RGK: f64 = 2000.0;
    const V0: f64 = -0.1;
    const S: f64 = 0.15;
    let a = (vgk - V0) / S;
    S / RGK * if a > 30.0 { a } else { a.exp().ln_1p() }
}

/// One stage's parts. Resistances in ohms, capacitances in farads; 0 means "not fitted".
#[derive(Clone, Copy)]
struct Circuit {
    /// Supply, plate load, cathode resistor and its bypass cap.
    vb: f64,
    rp: f64,
    rk: f64,
    ck: f64,
    /// Input network: source impedance, coupling cap (0 = direct), series resistor,
    /// shunt to ground, grid stopper, grid-to-ground (Miller) capacitance.
    ro: f64,
    cc: f64,
    rs: f64,
    rg: f64,
    rgs: f64,
    cm: f64,
    /// Low-pass on the plate (a plate bypass cap), Hz; 0 = none.
    plate_lp: f64,
}

struct Stage {
    c: Circuit,
    /// Backward-Euler companions: cathode network resistance, Ck/T, T/Cc, Cm/T.
    ak: f64,
    ck_t: f64,
    cc_t: f64,
    cm_t: f64,
    /// Input network as seen from the grid (see `set_input`): 1 / rc, the source's share that
    /// reaches the grid over the resistance it arrives through, and that resistance with the
    /// Miller cap's companion in parallel.
    inv_rc: f64,
    k_th: f64,
    rth2: f64,
    /// Operating point.
    ip_q: f64,
    vk_q: f64,
    /// [plate current on the load line, grid current] against grid-to-cathode volts.
    tab: Vec<[f64; 2]>,
    /// State: cathode, coupling-cap and grid voltages, last solution.
    vk: f64,
    vc: f64,
    vg: f64,
    x: f64,
    lp: OnePole,
}

impl Stage {
    fn new(fs: f64, c: Circuit) -> Self {
        let t = 1.0 / fs;
        let ck_t = c.ck / t;
        let ak = 1.0 / (1.0 / c.rk + ck_t);

        // operating point: no signal, the grid at ground, the cathode at Rk * Ip
        let (mut lo, mut hi) = (0.0, c.vb / (c.rp + c.rk));
        for _ in 0..60 {
            let ip = 0.5 * (lo + hi);
            if ip > koren(c.vb - (c.rp + c.rk) * ip, -c.rk * ip) {
                hi = ip;
            } else {
                lo = ip;
            }
        }
        let vk_q = c.rk * 0.5 * (lo + hi);
        // the part of the cathode voltage held by the bypass cap (zero without one)
        let bk_q = vk_q * ak * ck_t;

        // plate current where the valve's curve for each grid voltage meets the load line
        let rl = c.rp + ak;
        let v0 = c.vb - bk_q;
        let tab = (0..=TAB_N)
            .map(|i| {
                let x = X_MIN + i as f64 * X_STEP;
                let (mut lo, mut hi) = (0.0, v0 / rl);
                for _ in 0..40 {
                    let ip = 0.5 * (lo + hi);
                    if ip > koren(v0 - rl * ip, x) {
                        hi = ip;
                    } else {
                        lo = ip;
                    }
                }
                [0.5 * (lo + hi), grid_current(x)]
            })
            .collect();

        let mut lp = OnePole::default();
        if c.plate_lp > 0.0 {
            lp.set_low_pass(fs, c.plate_lp);
        }
        let mut s = Stage {
            c,
            ak,
            ck_t,
            cc_t: if c.cc > 0.0 { t / c.cc } else { 0.0 },
            cm_t: c.cm / t,
            inv_rc: 0.0,
            k_th: 0.0,
            rth2: 0.0,
            ip_q: 0.0,
            vk_q,
            tab,
            vk: 0.0,
            vc: 0.0,
            vg: 0.0,
            x: 0.0,
            lp,
        };
        s.ip_q = s.lookup(-vk_q).0;
        s.set_input(c.rs, c.rg);
        s.reset();
        s
    }

    /// Sets the attenuator in front of the grid: `rs` in series after the coupling cap,
    /// `rg` from there to ground.
    fn set_input(&mut self, rs: f64, rg: f64) {
        let rc = self.c.ro + rs + self.cc_t;
        let rth = rc * rg / (rc + rg) + self.c.rgs;
        self.inv_rc = 1.0 / rc;
        self.k_th = rg / (rc + rg) / rth;
        self.rth2 = 1.0 / (self.cm_t + 1.0 / rth);
    }

    fn reset(&mut self) {
        self.vk = self.vk_q;
        self.vc = 0.0;
        self.vg = 0.0;
        self.x = -self.vk_q;
        self.lp.reset();
    }

    /// Which straight piece of the tables `x` falls on (-1 below them, where they are flat).
    #[inline]
    fn segment(x: f64) -> isize {
        let p = (x - X_MIN) * (1.0 / X_STEP);
        if p <= 0.0 {
            -1
        } else {
            (p as usize).min(TAB_N - 1) as isize
        }
    }

    /// (plate current, grid current, their slopes, the table segment) at grid-to-cathode voltage `x`.
    #[inline]
    fn lookup(&self, x: f64) -> (f64, f64, f64, f64, isize) {
        let p = (x - X_MIN) * (1.0 / X_STEP);
        if p <= 0.0 {
            let e = self.tab[0];
            return (e[0], e[1], 0.0, 0.0, -1);
        }
        let i = (p as usize).min(TAB_N - 1);
        let f = p - i as f64; // above the table this exceeds 1: linear extrapolation
        let (a, b) = (self.tab[i], self.tab[i + 1]);
        let (dl, dg) = (b[0] - a[0], b[1] - a[1]);
        (a[0] + f * dl, a[1] + f * dg, dl * (1.0 / X_STEP), dg * (1.0 / X_STEP), i as isize)
    }

    /// Takes the previous stage's plate signal in volts, returns this plate's signal in volts.
    #[inline]
    fn process(&mut self, vin: f64) -> f64 {
        // everything in front of the grid, reduced to a source `vth` behind a resistance `r`
        let e = vin - self.vc;
        let r = self.rth2;
        let vth = r * (e * self.k_th + self.cm_t * self.vg);
        // the cathode sits at ak * (ip + ig) + bk
        let ak = self.ak;
        let bk = ak * self.ck_t * self.vk;

        // solve x + ak * (ip(x) + ig(x)) + r * ig(x) = vth - bk for x. The left side rises
        // monotonically and the tables are piecewise linear, so a Newton step that stays on the
        // same piece lands exactly on the answer; otherwise it is kept inside a bracket and
        // falls back to bisection.
        let target = vth - bk;
        let (mut lo, mut hi) = (f64::NEG_INFINITY, target);
        let mut x = self.x.min(target);
        let (mut ip, mut ig);
        let mut n = 0;
        loop {
            let (l, g, dl, dg, seg) = self.lookup(x);
            (ip, ig) = (l, g);
            let f = x + ak * (l + g) + r * g - target;
            n += 1;
            if f.abs() < 1e-9 || n == 32 {
                break;
            }
            if f > 0.0 {
                hi = x;
            } else {
                lo = x;
            }
            let xn = x - f / (1.0 + ak * (dl + dg) + r * dg);
            if xn > lo && xn < hi {
                if Self::segment(xn) == seg {
                    (ip, ig) = (l + dl * (xn - x), g + dg * (xn - x));
                    x = xn;
                    break;
                }
                x = xn;
            } else {
                if lo == f64::NEG_INFINITY {
                    // the currents only grow with x, so this is at or below the answer
                    let (l0, g0, ..) = self.lookup(target);
                    lo = target - ak * (l0 + g0) - r * g0;
                }
                x = 0.5 * (lo + hi);
            }
        }
        self.x = x;

        let vk = ak * (ip + ig) + bk;
        let vg = x + vk;
        // grid current and the Miller cap's current both flow through the grid stopper and
        // charge the coupling cap: this is what shifts the bias after a hard transient
        let i_in = ig + self.cm_t * (vg - self.vg);
        let va = vg + self.c.rgs * i_in;
        self.vc += self.cc_t * (e - va) * self.inv_rc;
        self.vk = vk;
        self.vg = vg;

        let y = -self.c.rp * (ip - self.ip_q);
        if self.c.plate_lp > 0.0 {
            self.lp.low_pass(y)
        } else {
            y
        }
    }
}

/// The GAIN pot between the first two stages.
const GAIN_POT: f64 = 500e3;
/// The resistor in series with it.
const GAIN_SERIES: f64 = 470e3;

pub struct HiGain {
    fs: f64,
    stages: Vec<Stage>,
    bright: Biquad,
    dc: OnePole,
    /// Smoothed GAIN knob, -1 until first set.
    gain: f64,
    last_bright: Option<bool>,
}

impl HiGain {
    /// `fs` is the rate `process` is called at (the oversampled rate).
    pub fn new(fs: f64) -> Self {
        let circuits = [
            // input stage: the guitar straight into the grid through a 68k stopper
            Circuit { vb: 360.0, rp: 220e3, rk: 1.8e3, ck: 0.47e-6, ro: 10e3, cc: 0.0, rs: 0.0, rg: 1e6, rgs: 68e3, cm: 130e-12, plate_lp: 0.0 },
            // second stage, behind the GAIN pot (`set` replaces rs and rg)
            Circuit { vb: 350.0, rp: 100e3, rk: 1.8e3, ck: 0.47e-6, ro: 49e3, cc: 22e-9, rs: GAIN_SERIES, rg: GAIN_POT, rgs: 0.0, cm: 120e-12, plate_lp: 0.0 },
            // cold clipper: a 39k unbypassed cathode puts it near cutoff, so it clips one side early
            Circuit { vb: 350.0, rp: 100e3, rk: 39e3, ck: 0.0, ro: 40e3, cc: 22e-9, rs: 470e3, rg: 1e6, rgs: 0.0, cm: 20e-12, plate_lp: 2000.0 },
            // fourth stage, behind a 220k grid stopper
            Circuit { vb: 360.0, rp: 220e3, rk: 1.8e3, ck: 0.47e-6, ro: 98e3, cc: 22e-9, rs: 0.0, rg: 330e3, rgs: 220e3, cm: 130e-12, plate_lp: 0.0 },
            // last stage, padded down so it adds its own clipping on top instead of flattening everything;
            // its grid stopper and Miller capacitance round off the edges the fourth stage hands it
            Circuit { vb: 378.0, rp: 220e3, rk: 2.2e3, ck: 22e-6, ro: 1e3, cc: 22e-9, rs: 470e3, rg: 15e3, rgs: 220e3, cm: 130e-12, plate_lp: 0.0 },
        ];
        let mut dc = OnePole::default();
        dc.set_low_pass(fs, 10.0);
        HiGain { fs, stages: circuits.iter().map(|c| Stage::new(fs, *c)).collect(), bright: Biquad::default(), dc, gain: -1.0, last_bright: None }
    }

    /// Back to the no-signal operating point (called when the channel is selected).
    pub fn reset(&mut self) {
        for s in self.stages.iter_mut() {
            s.reset();
        }
        self.bright.reset();
        self.dc.reset();
        self.gain = -1.0;
    }

    /// Once per block of `n` base-rate samples: the GAIN knob (0..1) and the BRIGHT switch.
    pub fn set(&mut self, gain: f64, bright: bool, n: usize, base_rate: f64) {
        if self.last_bright != Some(bright) {
            self.last_bright = Some(bright);
            self.bright.high_shelf(self.fs, 2200.0, if bright { 5.0 } else { 0.0 });
        }
        let prev = self.gain;
        if prev < 0.0 {
            self.gain = gain;
        } else {
            self.gain += (1.0 - (-(n as f64) / (base_rate * 0.02)).exp()) * (gain - prev);
        }
        if (self.gain - prev).abs() > 1e-7 {
            // audio-taper pot: the wiper's share of the track
            let w = self.gain.clamp(0.0, 1.0).powf(2.2).max(0.002);
            self.stages[1].set_input(GAIN_SERIES + (1.0 - w) * GAIN_POT, w * GAIN_POT);
        }
    }

    /// One oversampled sample, in volts at the input jack; the result is at the level the
    /// tone stack expects.
    #[inline]
    pub fn process(&mut self, v: f64) -> f64 {
        let mut v = self.stages[0].process(v);
        v = self.bright.process(v);
        for s in self.stages[1..].iter_mut() {
            v = s.process(v);
        }
        self.dc.high_pass(v) * OUT_SCALE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const FS: f64 = 192000.0;

    #[test]
    fn operating_points_are_sane() {
        let hg = HiGain::new(FS);
        for (i, s) in hg.stages.iter().enumerate() {
            let vp = s.c.vb - s.c.rp * s.ip_q;
            println!("stage {}: Ip {:.3} mA, Vk {:.2} V, Vp {:.1} V", i + 1, s.ip_q * 1e3, s.vk_q, vp);
            assert!(s.ip_q > 0.0 && vp > 0.3 * s.c.vb && vp < 0.99 * s.c.vb, "stage {} operating point", i + 1);
        }
        // the cold clipper idles far closer to cutoff than the others
        assert!(hg.stages[2].ip_q < 0.3 * hg.stages[1].ip_q);
    }

    #[test]
    fn first_stage_small_signal_gain() {
        let mut hg = HiGain::new(FS);
        let s = &mut hg.stages[0];
        // peak-to-peak, so the few millivolts of grid-leak bias at the plate don't count
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for n in 0..(FS as usize / 5) {
            let y = s.process(0.001 * (2.0 * PI * 1000.0 * n as f64 / FS).sin());
            if n > FS as usize / 10 {
                lo = lo.min(y);
                hi = hi.max(y);
            }
        }
        let gain = (hi - lo) / 0.002;
        println!("stage 1 gain at 1 kHz: {gain:.1}");
        assert!((30.0..90.0).contains(&gain));
    }

    #[test]
    fn silence_stays_silent() {
        let mut hg = HiGain::new(FS);
        hg.set(1.0, false, 256, 48000.0);
        let mut peak: f64 = 0.0;
        for n in 0..FS as usize {
            let y = hg.process(0.0);
            if n > FS as usize / 2 {
                peak = peak.max(y.abs());
            }
        }
        println!("idle output: {peak:e}");
        assert!(peak < 1e-4);
    }

    #[test]
    fn hard_drive_is_bounded_and_recovers() {
        let mut hg = HiGain::new(FS);
        hg.set(1.0, true, 256, 48000.0);
        let (mut peak, mut sum): (f64, f64) = (0.0, 0.0);
        let n1 = FS as usize;
        for n in 0..n1 {
            let y = hg.process(2.0 * (2.0 * PI * 110.0 * n as f64 / FS).sin());
            assert!(y.is_finite());
            peak = peak.max(y.abs());
            sum += y * y;
        }
        println!("driven: peak {:.2}, rms {:.2}", peak, (sum / n1 as f64).sqrt());
        assert!(peak < 4.0 && sum / n1 as f64 > 0.01);
        // after the signal stops the bias recovers and the output returns to rest
        let mut tail: f64 = 0.0;
        for n in 0..2 * n1 {
            let y = hg.process(0.0);
            if n > n1 {
                tail = tail.max(y.abs());
            }
        }
        println!("tail after drive: {tail:e}");
        assert!(tail < 1e-3);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::dsp::power_amp::PowerAmp;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn timing() {
        let fs = 384000.0;
        let n = fs as usize * 4;
        // a decaying low note with some upper content, like a palm mute ringing out
        let sig: Vec<f64> = (0..n).map(|i| { let t = i as f64 / fs; 0.6 * (-t).exp() * ((2.0 * std::f64::consts::PI * 110.0 * t).sin() + 0.3 * (2.0 * std::f64::consts::PI * 990.0 * t).sin()) }).collect();
        let mut hg = HiGain::new(fs);
        hg.set(0.6, false, 256, 48000.0);
        let t0 = Instant::now();
        let pre: Vec<f64> = sig.iter().map(|x| hg.process(*x)).collect();
        let t_pre = t0.elapsed().as_secs_f64();
        let mut pa = PowerAmp::new(fs);
        pa.set(0.5, 0.4);
        let t0 = Instant::now();
        let mut acc = 0.0;
        for x in pre.iter() { acc += pa.process(*x * 0.7); }
        let t_pa = t0.elapsed().as_secs_f64();
        println!("preamp {:.0} ns/sample ({:.1}% of a core at 8x), power amp {:.0} ns/sample ({:.1}%) [{acc:.3}]", t_pre / n as f64 * 1e9, t_pre / 4.0 * 100.0, t_pa / n as f64 * 1e9, t_pa / 4.0 * 100.0);
    }
}
