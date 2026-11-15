//! Filters: RBJ biquad, one-pole, Marshall-style tone stack.

use std::f64::consts::PI;

/// RBJ cookbook biquad, transposed direct form II, double precision.
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Takes `o`'s coefficients, keeping this filter's state.
    pub fn copy_coefs(&mut self, o: &Biquad) {
        (self.b0, self.b1, self.b2, self.a1, self.a2) = (o.b0, o.b1, o.b2, o.a1, o.a2);
    }

    /// Gain in dB at `f` Hz (for drawing response curves).
    pub fn magnitude_db(&self, fs: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-30)).max(1e-30).log10()
    }

    fn set(&mut self, nb0: f64, nb1: f64, nb2: f64, na0: f64, na1: f64, na2: f64) {
        self.b0 = nb0 / na0;
        self.b1 = nb1 / na0;
        self.b2 = nb2 / na0;
        self.a1 = na1 / na0;
        self.a2 = na2 / na0;
    }

    /// Bilinear transform of the analog section (b0 + b1 s + b2 s^2) / (a0 + a1 s + a2 s^2).
    pub fn analog(&mut self, fs: f64, b: [f64; 3], a: [f64; 3]) {
        let k = 2.0 * fs;
        let z = |c: [f64; 3]| (c[0] + c[1] * k + c[2] * k * k, 2.0 * (c[0] - c[2] * k * k), c[0] - c[1] * k + c[2] * k * k);
        let ((b0, b1, b2), (a0, a1, a2)) = (z(b), z(a));
        self.set(b0, b1, b2, a0, a1, a2);
    }

    pub fn low_pass(&mut self, fs: f64, f: f64, q: f64) {
        let w = 2.0 * PI * f / fs;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    pub fn high_pass(&mut self, fs: f64, f: f64, q: f64) {
        let w = 2.0 * PI * f / fs;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    /// Band-pass with 0 dB gain at the centre.
    pub fn band_pass(&mut self, fs: f64, f: f64, q: f64) {
        let w = 2.0 * PI * f / fs;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set(al, 0.0, -al, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    pub fn peak(&mut self, fs: f64, f: f64, q: f64, db: f64) {
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * f / fs;
        let (c, al) = (w.cos(), w.sin() / (2.0 * q));
        self.set(1.0 + al * a, -2.0 * c, 1.0 - al * a, 1.0 + al / a, -2.0 * c, 1.0 - al / a);
    }

    /// Peaking bell matched to the analog prototype (Vicanek, "Matched Second Order Digital
    /// Filters", 2016): impulse-invariant poles, zeros solved so the gain is exactly `db` at `f`
    /// with the peak sitting there. Unlike the bilinear `peak`, bells near Nyquist keep their
    /// analog width and shape instead of being squeezed, so the top bands stay open and airy.
    pub fn peak_matched(&mut self, fs: f64, f: f64, q: f64, db: f64) {
        let g = 10f64.powf(db / 20.0);
        let w = 2.0 * PI * f / fs;
        // RBJ-symmetric prototype: pole Q = Q·√G, so boosts and cuts mirror each other
        let z = 1.0 / (2.0 * q * g.sqrt());
        let e = (-z * w).exp();
        let a1 = if z <= 1.0 { -2.0 * e * ((1.0 - z * z).sqrt() * w).cos() } else { -2.0 * e * ((z * z - 1.0).sqrt() * w).cosh() };
        let a2 = e * e;
        let (ca0, ca1, ca2) = ((1.0 + a1 + a2).powi(2), (1.0 - a1 + a2).powi(2), -4.0 * a2);
        let p1 = (w / 2.0).sin().powi(2);
        let p0 = 1.0 - p1;
        let p2 = 4.0 * p0 * p1;
        let g2 = g * g;
        let r1 = (ca0 * p0 + ca1 * p1 + ca2 * p2) * g2;
        let r2 = (-ca0 + ca1 + 4.0 * (p0 - p1) * ca2) * g2;
        let cb0 = ca0;
        let cb2 = (r1 - r2 * p1 - cb0) / (4.0 * p1 * p1);
        let cb1 = r2 + cb0 + 4.0 * (p1 - p0) * cb2;
        let (s0, s1) = (cb0.sqrt(), cb1.max(0.0).sqrt());
        let wq = 0.5 * (s0 + s1);
        let b0 = 0.5 * (wq + (wq * wq + cb2).max(0.0).sqrt());
        let b1 = 0.5 * (s0 - s1);
        let b2 = -cb2 / (4.0 * b0);
        self.set(b0, b1, b2, 1.0, a1, a2);
    }

    pub fn low_shelf(&mut self, fs: f64, f: f64, db: f64) {
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * f / fs;
        let (c, s) = (w.cos(), w.sin());
        let al = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * al;
        self.set(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        );
    }

    pub fn high_shelf(&mut self, fs: f64, f: f64, db: f64) {
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * f / fs;
        let (c, s) = (w.cos(), w.sin());
        let al = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * al;
        self.set(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        );
    }
}

/// One-pole low/high pass.
#[derive(Clone, Copy, Default)]
pub struct OnePole {
    a: f64,
    z: f64,
}

impl OnePole {
    pub fn set_low_pass(&mut self, fs: f64, f: f64) {
        self.a = 1.0 - (-2.0 * PI * f / fs).exp();
    }
    #[inline]
    pub fn low_pass(&mut self, x: f64) -> f64 {
        self.z += self.a * (x - self.z);
        self.z
    }
    #[inline]
    pub fn high_pass(&mut self, x: f64) -> f64 {
        x - self.low_pass(x)
    }
    pub fn reset(&mut self) {
        self.z = 0.0;
    }
}

/// Marshall-style TMB tone stack, discretized with the bilinear transform
/// (Yeh & Smith, "Discretization of the '59 Fender Bassman Tone Stack", DAFx 2006).
pub struct ToneStack {
    fs: f64,
    b: [f64; 4],
    a: [f64; 3],
    x1: f64,
    x2: f64,
    x3: f64,
    y1: f64,
    y2: f64,
    y3: f64,
    last: (f64, f64, f64),
    parts: [f64; 7],
}

impl ToneStack {
    /// Component values as [treble pot, bass pot, mid pot, slope resistor, treble cap, bass cap,
    /// mid cap]. JCM800-ish: the clean, crunch and lead channels.
    pub const JCM800: [f64; 7] = [220e3, 1e6, 22e3, 33e3, 470e-12, 22e-9, 22e-9];
    /// The SLO-family stack that goes with the LEAD II preamp: a larger slope resistor, so the
    /// mids scoop deeper.
    pub const SLO: [f64; 7] = [250e3, 1e6, 25e3, 47e3, 470e-12, 22e-9, 22e-9];

    pub fn new(sample_rate: f64) -> Self {
        Self::with_parts(sample_rate, Self::JCM800)
    }

    pub fn with_parts(sample_rate: f64, parts: [f64; 7]) -> Self {
        let mut t = ToneStack { fs: sample_rate, b: [0.0; 4], a: [0.0; 3], x1: 0.0, x2: 0.0, x3: 0.0, y1: 0.0, y2: 0.0, y3: 0.0, last: (-1.0, -1.0, -1.0), parts };
        t.set(0.5, 0.5, 0.5);
        t
    }

    /// All 0..1.
    pub fn set(&mut self, treble: f64, mid: f64, bass: f64) {
        if (treble, mid, bass) == self.last {
            return;
        }
        self.last = (treble, mid, bass);
        let [r1, r2, r3, r4, c1, c2, c3] = self.parts;
        let (t, m, l) = (treble, mid, ((bass - 1.0) * 3.5).exp()); // log taper bass pot

        let b1 = t * c1 * r1 + m * c3 * r3 + l * (c1 * r2 + c2 * r2) + (c1 * r3 + c2 * r3);
        let b2 = t * (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4) - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + m * (c1 * c3 * r1 * r3 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4)
            + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
            + (c1 * c2 * r1 * r3 + c1 * c2 * r3 * r4 + c1 * c3 * r3 * r4);
        let b3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
            - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + t * c1 * c2 * c3 * r1 * r3 * r4
            - t * m * c1 * c2 * c3 * r1 * r3 * r4
            + t * l * c1 * c2 * c3 * r1 * r2 * r4;
        let a0 = 1.0;
        let a1 = (c1 * r1 + c1 * r3 + c2 * r3 + c2 * r4 + c3 * r4) + m * c3 * r3 + l * (c1 * r2 + c2 * r2);
        let a2 = m * (c1 * c3 * r1 * r3 - c2 * c3 * r3 * r4 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
            - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + l * (c1 * c2 * r2 * r4 + c1 * c2 * r1 * r2 + c1 * c3 * r2 * r4 + c2 * c3 * r2 * r4)
            + (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4 + c1 * c2 * r3 * r4 + c1 * c2 * r1 * r3 + c1 * c3 * r3 * r4 + c2 * c3 * r3 * r4);
        let a3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
            - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + m * (c1 * c2 * c3 * r3 * r3 * r4 + c1 * c2 * c3 * r1 * r3 * r3 - c1 * c2 * c3 * r1 * r3 * r4)
            + l * c1 * c2 * c3 * r1 * r2 * r4
            + c1 * c2 * c3 * r1 * r3 * r4;

        let c = 2.0 * self.fs;
        let (cc2, cc3) = (c * c, c * c * c);
        let nb0 = -b1 * c - b2 * cc2 - b3 * cc3;
        let nb1 = -b1 * c + b2 * cc2 + 3.0 * b3 * cc3;
        let nb2 = b1 * c + b2 * cc2 - 3.0 * b3 * cc3;
        let nb3 = b1 * c - b2 * cc2 + b3 * cc3;
        let na0 = -a0 - a1 * c - a2 * cc2 - a3 * cc3;
        let na1 = -3.0 * a0 - a1 * c + a2 * cc2 + 3.0 * a3 * cc3;
        let na2 = -3.0 * a0 + a1 * c + a2 * cc2 - 3.0 * a3 * cc3;
        let na3 = -a0 + a1 * c - a2 * cc2 + a3 * cc3;
        self.b = [nb0 / na0, nb1 / na0, nb2 / na0, nb3 / na0];
        self.a = [na1 / na0, na2 / na0, na3 / na0];
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x1 + self.b[2] * self.x2 + self.b[3] * self.x3
            - self.a[0] * self.y1
            - self.a[1] * self.y2
            - self.a[2] * self.y3;
        self.x3 = self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y3 = self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain of the stack at `hz` with all three knobs at noon.
    fn gain(parts: [f64; 7], hz: f64) -> f64 {
        let fs = 384000.0;
        let mut t = ToneStack::with_parts(fs, parts);
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for n in 0..fs as usize / 4 {
            let y = t.process((2.0 * PI * hz * n as f64 / fs).sin());
            if n > fs as usize / 8 {
                lo = lo.min(y);
                hi = hi.max(y);
            }
        }
        (hi - lo) / 2.0
    }

    #[test]
    fn slo_stack_scoops_deeper_than_jcm800() {
        for (name, parts) in [("JCM800", ToneStack::JCM800), ("SLO", ToneStack::SLO)] {
            let g: Vec<String> = [100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0].iter().map(|f| format!("{:+.1}", 20.0 * gain(parts, *f).log10())).collect();
            println!("{name}: 100/250/500/1k/2.5k/5k Hz = {} dB", g.join(" / "));
        }
        assert!(gain(ToneStack::SLO, 500.0) < gain(ToneStack::JCM800, 500.0));
    }
}
