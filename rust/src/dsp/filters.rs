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
}

impl ToneStack {
    // JCM800-ish component values
    const R1: f64 = 220e3;
    const R2: f64 = 1e6;
    const R3: f64 = 22e3;
    const R4: f64 = 33e3;
    const C1: f64 = 470e-12;
    const C2: f64 = 22e-9;
    const C3: f64 = 22e-9;

    pub fn new(sample_rate: f64) -> Self {
        let mut t = ToneStack { fs: sample_rate, b: [0.0; 4], a: [0.0; 3], x1: 0.0, x2: 0.0, x3: 0.0, y1: 0.0, y2: 0.0, y3: 0.0, last: (-1.0, -1.0, -1.0) };
        t.set(0.5, 0.5, 0.5);
        t
    }

    /// All 0..1.
    pub fn set(&mut self, treble: f64, mid: f64, bass: f64) {
        if (treble, mid, bass) == self.last {
            return;
        }
        self.last = (treble, mid, bass);
        let (r1, r2, r3, r4, c1, c2, c3) = (Self::R1, Self::R2, Self::R3, Self::R4, Self::C1, Self::C2, Self::C3);
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
