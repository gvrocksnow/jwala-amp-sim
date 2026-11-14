//! The LEAD II power amp: phase inverter, a cold-biased push-pull pair, output transformer and
//! speaker load, wrapped in a global negative-feedback loop that holds the PRESENCE and DEPTH
//! controls.
//!
//! Power valves are current sources, so without feedback the voltage on the speaker follows
//! its impedance: a bump at the cabinet resonance and a rise towards the treble. Feedback
//! flattens that. PRESENCE and DEPTH work by taking feedback away above and below the mids,
//! which lets the speaker's own shape come back there. Once the valves clip there is no loop
//! gain left, so the feedback (and with it both controls) lets go: the amp gets looser and
//! brighter exactly when it is pushed. A plain EQ in front of a clipper cannot do that.
//!
//! The loop has no delay in it. Per sample the forward path is a rising function of the
//! error voltage at the phase inverter, and every filter's output is its input times a
//! constant plus a term from its state, so the loop closes by solving one scalar equation.
//!
//! Levels: input and output are normalised so that 1.0 is the clipping point, as for the
//! other channels' power stage. Inside, grid voltages are in volts.

use super::filters::OnePole;
use std::f64::consts::PI;

/// Phase inverter: volts of drive per unit of error, and the most it can swing per side.
const PI_GAIN: f64 = 190.0;
const PI_SWING: f64 = 70.0;
/// One side of the phase inverter has a little less gain than the other, as in a real
/// long-tailed pair; the mismatch is what leaves some even harmonics after the push-pull.
const PI_BALANCE: f64 = 0.95;
/// Power valve bias and cutoff (volts below the cathode). The 8 V between them is a cold
/// bias: about 5% of peak current at idle, so there is a crossover region.
const BIAS: f64 = 52.0;
const CUTOFF: f64 = 60.0;
/// Speaker load: mid-band volts per unit of valve current, the height of the resonance bump
/// and of the inductive rise above it.
const LOAD: f64 = 1.15;
const SPK_RES_HZ: f64 = 100.0;
const SPK_RES_Q: f64 = 1.5;
const SPK_RES: f64 = 1.5;
const SPK_RISE_HZ: f64 = 4000.0;
const SPK_RISE: f64 = 2.0;
/// Fraction of the output fed back to the phase inverter.
const FEEDBACK: f64 = 0.75;
/// The most of that feedback PRESENCE and DEPTH can take away, and where they act.
const TONE_RANGE: f64 = 0.8;
const PRESENCE_HZ: f64 = 1200.0;
const DEPTH_HZ: f64 = 250.0;
/// Supply droop at full continuous current.
const SAG: f64 = 0.10;
/// Bound on the forward path's output, for bracketing the solve.
const OUT_MAX: f64 = 1.05;

/// (softplus, its slope): a smooth max(a, 0) with a knee `w` wide.
#[inline]
fn softplus(a: f64, w: f64) -> (f64, f64) {
    let t = a / w;
    if t > 30.0 {
        (a, 1.0)
    } else if t < -30.0 {
        (0.0, 0.0)
    } else {
        let e = t.exp();
        (w * e.ln_1p(), e / (1.0 + e))
    }
}

/// (soft clip to +-1, its slope), gentle knee: the phase inverter.
#[inline]
fn clip_soft(x: f64) -> (f64, f64) {
    let r = 1.0 / (1.0 + x * x).sqrt();
    (x * r, r * r * r)
}

/// (soft clip to +-1, its slope), sharp knee: the valves running out of plate voltage.
#[inline]
fn clip_hard(x: f64) -> (f64, f64) {
    let q = 1.0 + x * x * x * x;
    let r = 1.0 / q.sqrt().sqrt();
    (x * r, r / q)
}

/// Trapezoidal one-pole low-pass whose output is available as `gain * input + state term`
/// before the input is known.
#[derive(Default, Clone, Copy)]
struct Pole {
    g: f64,
    s: f64,
}

impl Pole {
    fn set(&mut self, fs: f64, hz: f64) {
        self.g = (PI * hz / fs).tan();
    }
    /// The low-pass output will be `.0 * x + .1`.
    #[inline]
    fn peek(&self) -> (f64, f64) {
        (self.g / (1.0 + self.g), self.s / (1.0 + self.g))
    }
    #[inline]
    fn push(&mut self, x: f64) {
        let v = (x - self.s) * self.g / (1.0 + self.g);
        self.s += 2.0 * v;
    }
}

/// Trapezoidal state-variable band-pass (unity gain at the centre), same idea.
#[derive(Default, Clone, Copy)]
struct Band {
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
    ic1: f64,
    ic2: f64,
}

impl Band {
    fn set(&mut self, fs: f64, hz: f64, q: f64) {
        let g = (PI * hz / fs).tan();
        self.k = 1.0 / q;
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }
    #[inline]
    fn peek(&self) -> (f64, f64) {
        (self.k * self.a2, self.k * (self.a1 * self.ic1 - self.a2 * self.ic2))
    }
    #[inline]
    fn push(&mut self, x: f64) {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
    }
}

/// Everything the forward path produces for one error voltage.
struct Fwd {
    /// Speaker voltage and its slope against the error voltage.
    y: f64,
    dy: f64,
    /// How far each grid was driven past the point where it conducts (volts).
    over: [f64; 2],
    /// Valve currents.
    i: [f64; 2],
}

pub struct PowerAmp {
    fs: f64,
    spk_res: Band,
    spk_rise: Pole,
    pres: Pole,
    dep: Pole,
    kp: f64,
    kd: f64,
    /// Charge on the two coupling caps into the power valve grids: grid current pushes the
    /// bias colder after the valves have been driven hard, and it leaks back over a few ms.
    vc: [f64; 2],
    cap_charge: f64,
    cap_leak: f64,
    /// Supply droop, the same seen more slowly by the preamp's rail, and its smoothing.
    droop: f64,
    droop_slow: f64,
    droop_k: f64,
    droop_slow_k: f64,
    /// Output transformer core flux and the share of the output it takes when pushed further.
    flux: f64,
    flux_leak: f64,
    flux_sat: f64,
    loss: f64,
    ot: OnePole,
    /// Last solution.
    e: f64,
}

impl PowerAmp {
    /// `fs` is the rate `process` is called at (the oversampled rate).
    pub fn new(fs: f64) -> Self {
        let t = 1.0 / fs;
        let (mut spk_res, mut spk_rise, mut pres, mut dep) = (Band::default(), Pole::default(), Pole::default(), Pole::default());
        spk_res.set(fs, SPK_RES_HZ, SPK_RES_Q);
        spk_rise.set(fs, SPK_RISE_HZ);
        pres.set(fs, PRESENCE_HZ);
        dep.set(fs, DEPTH_HZ);
        let mut ot = OnePole::default();
        ot.set_low_pass(fs, 18000.0);
        PowerAmp {
            fs,
            spk_res,
            spk_rise,
            pres,
            dep,
            kp: 0.0,
            kd: 0.0,
            vc: [0.0; 2],
            // 22 nF fed from ~91k charges in 2 ms and leaks through the 220k grid resistor in 4.8 ms
            cap_charge: t / 0.002,
            cap_leak: t / 0.0048,
            droop: 0.0,
            droop_slow: 0.0,
            droop_k: 1.0 - (-t / 0.025).exp(),
            droop_slow_k: 1.0 - (-t / 0.15).exp(),
            flux: 0.0,
            flux_leak: t * 2.0 * PI * 8.0,
            // the core starts to saturate around a full-level 45 Hz sine
            flux_sat: 1.0 / (2.0 * PI * 45.0),
            loss: 0.0,
            ot,
            e: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = PowerAmp { kp: self.kp, kd: self.kd, ..PowerAmp::new(self.fs) };
    }

    /// PRESENCE and DEPTH, 0..1.
    pub fn set(&mut self, presence: f64, depth: f64) {
        self.kp = TONE_RANGE * presence.clamp(0.0, 1.0);
        self.kd = TONE_RANGE * depth.clamp(0.0, 1.0);
    }

    /// What the supply droop leaves of the preamp's rail (1.0 at rest).
    #[inline]
    pub fn rail(&self) -> f64 {
        1.0 - 0.5 * self.droop_slow
    }

    /// Phase inverter -> grids -> valves -> speaker load -> supply limit, for error voltage `e`.
    /// `z0` and `sz` are the speaker load's gain and state term for this sample.
    #[inline]
    fn forward(&self, e: f64, z0: f64, sz: f64) -> Fwd {
        let (d, dd) = clip_soft(e * (PI_GAIN / PI_SWING));
        let (d, dd) = (d * PI_SWING, dd * PI_GAIN);
        // screens sag with the supply, and the valves pass less current for the same grid voltage
        let headroom = 1.0 - self.droop;
        let screen = headroom * headroom.sqrt();
        let mut f = Fwd { y: 0.0, dy: 0.0, over: [0.0; 2], i: [0.0; 2] };
        let mut di = 0.0;
        for (k, sign) in [(0, 1.0), (1, -PI_BALANCE)] {
            // drive at the grid, less what has collected on its coupling cap
            let u = sign * d - self.vc[k];
            // the grid cannot go above the cathode: past that it conducts and clamps
            let (over, dover) = softplus(u - BIAS, 1.5);
            let vg = u - over - BIAS;
            // three-halves power law above cutoff
            let (s, ds) = softplus(vg + CUTOFF, 2.0);
            let r = (s / CUTOFF).sqrt();
            f.over[k] = over;
            f.i[k] = screen * r * s / CUTOFF;
            di += sign.abs() * screen * 1.5 * r / CUTOFF * ds * (1.0 - dover) * dd;
        }
        let v = z0 * (f.i[0] - f.i[1]) + sz;
        let (c, dc) = clip_hard(v / headroom);
        // a core near saturation only resists being pushed further the same way
        let keep = if c * self.flux > 0.0 { 1.0 - self.loss } else { 1.0 };
        f.y = headroom * c * keep;
        f.dy = dc * z0 * di * keep;
        f
    }

    /// One oversampled sample.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        // the filters around the loop, each as gain * input + state term
        let (gb, sb) = self.spk_res.peek();
        let (gh, sh) = self.spk_rise.peek();
        let z0 = LOAD * (1.0 + SPK_RES * gb + SPK_RISE * (1.0 - gh));
        let sz = LOAD * (SPK_RES * sb - SPK_RISE * sh);
        let (gp, sp) = self.pres.peek();
        let (gd, sd) = self.dep.peek();
        let g0 = FEEDBACK * (1.0 - self.kp * (1.0 - gp) - self.kd * gd);
        let sg = FEEDBACK * (self.kp * sp - self.kd * sd);

        // error = input - feedback: e + g0 * y(e) = x - sg. The left side rises with e, so
        // Newton is kept inside a bracket and falls back to bisection.
        let target = x - sg;
        let (mut lo, mut hi) = (target - g0 * OUT_MAX, target + g0 * OUT_MAX);
        let mut e = self.e.clamp(lo, hi);
        let mut f = self.forward(e, z0, sz);
        for _ in 0..40 {
            let r = e + g0 * f.y - target;
            if r.abs() < 1e-10 {
                break;
            }
            if r > 0.0 {
                hi = e;
            } else {
                lo = e;
            }
            let en = e - r / (1.0 + g0 * f.dy);
            e = if en > lo && en < hi { en } else { 0.5 * (lo + hi) };
            f = self.forward(e, z0, sz);
        }
        self.e = e;
        let y = f.y;

        // advance the state
        let id = f.i[0] - f.i[1];
        self.spk_res.push(id);
        self.spk_rise.push(id);
        self.pres.push(y);
        self.dep.push(y);
        for k in 0..2 {
            self.vc[k] += self.cap_charge * f.over[k] - self.cap_leak * self.vc[k];
        }
        // the supply sags with the current drawn beyond idle
        let idle = 2.0 * ((CUTOFF - BIAS) / CUTOFF).powf(1.5);
        let draw = SAG * (f.i[0] + f.i[1] - idle).max(0.0);
        self.droop += self.droop_k * (draw - self.droop);
        self.droop_slow += self.droop_slow_k * (self.droop - self.droop_slow);
        // the transformer core integrates the output; near saturation it takes some of it back
        self.flux += y / self.fs - self.flux_leak * self.flux;
        let fl = (self.flux / self.flux_sat).abs();
        self.loss = (0.2 * fl * fl * fl).min(0.6);

        self.ot.low_pass(y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 192000.0;

    /// Peak output for a sine of amplitude `a` at `hz`, after settling.
    fn gain(pa: &mut PowerAmp, hz: f64, a: f64) -> f64 {
        pa.reset();
        let n = FS as usize / 2;
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for i in 0..n {
            let y = pa.process(a * (2.0 * PI * hz * i as f64 / FS).sin());
            assert!(y.is_finite());
            if i > n / 2 {
                lo = lo.min(y);
                hi = hi.max(y);
            }
        }
        (hi - lo) / (2.0 * a)
    }

    #[test]
    fn mid_band_gain_is_near_unity() {
        let mut pa = PowerAmp::new(FS);
        pa.set(0.5, 0.4);
        let g = gain(&mut pa, 500.0, 0.01);
        println!("gain at 500 Hz: {g:.2}");
        assert!((0.6..1.6).contains(&g));
    }

    #[test]
    fn presence_and_depth_act_through_the_loop() {
        let mut pa = PowerAmp::new(FS);
        let mut row = |p: f64, d: f64| {
            pa.set(p, d);
            let r = [gain(&mut pa, 100.0, 0.01), gain(&mut pa, 500.0, 0.01), gain(&mut pa, 5000.0, 0.01)];
            println!("presence {p} depth {d}: 100 Hz {:+.1} dB, 500 Hz {:+.1} dB, 5 kHz {:+.1} dB", 20.0 * r[0].log10(), 20.0 * r[1].log10(), 20.0 * r[2].log10());
            r
        };
        let (flat, pres, dep) = (row(0.0, 0.0), row(1.0, 0.0), row(0.0, 1.0));
        assert!(pres[2] > 2.0 * flat[2] && (pres[0] / flat[0] - 1.0).abs() < 0.2);
        assert!(dep[0] > 2.0 * flat[0] && (dep[2] / flat[2] - 1.0).abs() < 0.2);
    }

    #[test]
    fn controls_let_go_when_the_valves_clip() {
        // driven far into clipping, the output is set by the supply, not by the feedback network
        let mut pa = PowerAmp::new(FS);
        pa.set(0.0, 0.0);
        let a = gain(&mut pa, 5000.0, 8.0);
        pa.set(1.0, 0.0);
        let b = gain(&mut pa, 5000.0, 8.0);
        println!("clipped 5 kHz output, presence 0 vs 1: {:.3} vs {:.3}", a * 8.0, b * 8.0);
        assert!((b / a - 1.0).abs() < 0.25);
    }

    #[test]
    fn overdrive_is_bounded_and_recovers() {
        let mut pa = PowerAmp::new(FS);
        pa.set(0.5, 0.4);
        let mut peak: f64 = 0.0;
        for i in 0..FS as usize {
            let y = pa.process(20.0 * (2.0 * PI * 82.0 * i as f64 / FS).sin());
            assert!(y.is_finite());
            peak = peak.max(y.abs());
        }
        println!("overdriven peak {peak:.2}, cap bias shift {:.1} V, droop {:.3}", pa.vc[0], pa.droop);
        assert!(peak < OUT_MAX);
        let mut tail: f64 = 0.0;
        for i in 0..FS as usize {
            let y = pa.process(0.0);
            if i > FS as usize / 2 {
                tail = tail.max(y.abs());
            }
        }
        println!("tail {tail:e}");
        assert!(tail < 1e-4);
    }
}
