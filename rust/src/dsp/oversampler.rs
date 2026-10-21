//! Polyphase FIR up/down sampler (factor 4, Kaiser-windowed sinc) so the nonlinear
//! stages run at 4x and their harmonics don't alias back down.

use std::f64::consts::PI;

pub const FACTOR: usize = 4;
const TAPS: usize = 96; // multiple of FACTOR
const UP_LEN: usize = TAPS / FACTOR;

pub struct Oversampler {
    h: [f64; TAPS],
    up_hist: [f64; UP_LEN * 2],
    down_hist: [f64; TAPS * 2],
    up_pos: usize,
    down_pos: usize,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut s, mut t) = (1.0, 1.0);
    for k in 1..30 {
        t *= x / (2.0 * k as f64);
        s += t * t;
    }
    s
}

impl Default for Oversampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Oversampler {
    pub fn new() -> Self {
        let fc = 0.45 / FACTOR as f64; // cutoff relative to the oversampled rate
        let beta = 9.0;
        let mut h = [0.0; TAPS];
        let mut sum = 0.0;
        for (i, hi) in h.iter_mut().enumerate() {
            let n = i as f64 - (TAPS - 1) as f64 / 2.0;
            let sinc = if n == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * n).sin() / (PI * n) };
            let r = 2.0 * i as f64 / (TAPS - 1) as f64 - 1.0;
            *hi = sinc * bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta);
            sum += *hi;
        }
        for hi in h.iter_mut() {
            *hi /= sum;
        }
        Oversampler { h, up_hist: [0.0; UP_LEN * 2], down_hist: [0.0; TAPS * 2], up_pos: 0, down_pos: 0 }
    }

    /// Takes one base-rate sample, writes FACTOR oversampled samples.
    #[inline]
    pub fn up(&mut self, x: f64, out: &mut [f64; FACTOR]) {
        self.up_pos = (self.up_pos + 1) % UP_LEN;
        self.up_hist[self.up_pos] = x;
        self.up_hist[self.up_pos + UP_LEN] = x;
        // newest sample is at up_hist[up_pos + UP_LEN]; walk backwards in time
        let newest = self.up_pos + UP_LEN;
        for (p, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for j in 0..UP_LEN {
                acc += self.h[j * FACTOR + p] * self.up_hist[newest - j];
            }
            *o = acc * FACTOR as f64;
        }
    }

    /// Takes FACTOR oversampled samples, returns one base-rate sample.
    #[inline]
    pub fn down(&mut self, inp: &[f64; FACTOR]) -> f64 {
        for &v in inp {
            self.down_pos = (self.down_pos + 1) % TAPS;
            self.down_hist[self.down_pos] = v;
            self.down_hist[self.down_pos + TAPS] = v;
        }
        let newest = self.down_pos + TAPS;
        let mut acc = 0.0;
        for j in 0..TAPS {
            acc += self.h[j] * self.down_hist[newest - j];
        }
        acc
    }
}
