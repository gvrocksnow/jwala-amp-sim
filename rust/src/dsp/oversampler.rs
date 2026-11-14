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

/// A further 2x on top of the 4x region, for the LEAD II amp only (its five cascaded stages
/// make far more harmonics than the other channels). Half-band FIR, 23 taps: every other tap
/// is zero and the centre is one half, so it costs twelve multiplies per sample pair. The signal
/// it carries is already band-limited by the 4x stage, which is why so short a filter is enough.
pub struct Doubler {
    /// The taps at odd offsets from the centre.
    h: [f64; HB],
    up_hist: [f64; HB],
    down_hist: [f64; 2 * HB - 1],
}

/// Number of non-zero taps either side of the centre, counted together.
const HB: usize = 12;

impl Default for Doubler {
    fn default() -> Self {
        Self::new()
    }
}

impl Doubler {
    pub fn new() -> Self {
        let beta = 8.2;
        let mut h = [0.0; HB];
        let mut sum = 0.0;
        for (j, hj) in h.iter_mut().enumerate() {
            let o = 2.0 * j as f64 - (HB - 1) as f64;
            let r = o / (HB - 1) as f64;
            *hj = (PI * o / 2.0).sin() / (PI * o) * bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta);
            sum += *hj;
        }
        // with the centre tap at one half, the rest must add up to the other half
        for hj in h.iter_mut() {
            *hj *= 0.5 / sum;
        }
        Doubler { h, up_hist: [0.0; HB], down_hist: [0.0; 2 * HB - 1] }
    }

    /// Takes one sample, returns two at twice the rate (earlier first).
    #[inline]
    pub fn up(&mut self, x: f64) -> [f64; 2] {
        self.up_hist.copy_within(0..HB - 1, 1);
        self.up_hist[0] = x;
        let mut acc = 0.0;
        for j in 0..HB {
            acc += self.h[j] * self.up_hist[j];
        }
        [2.0 * acc, self.up_hist[HB / 2 - 1]]
    }

    /// Takes two samples (earlier first), returns one at half the rate.
    #[inline]
    pub fn down(&mut self, a: f64, b: f64) -> f64 {
        self.down_hist.copy_within(0..2 * HB - 3, 2);
        self.down_hist[1] = a;
        self.down_hist[0] = b;
        let mut acc = 0.5 * self.down_hist[HB - 1];
        for j in 0..HB {
            acc += self.h[j] * self.down_hist[2 * j];
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain of an up-then-down round trip for a sine at `f` (cycles per sample), by RMS: the
    /// round trip delays by a whole number of samples plus a half, so peaks would mislead.
    fn round_trip(f: f64) -> f64 {
        let mut d = Doubler::new();
        let (mut sum, mut count) = (0.0, 0);
        for n in 0..40200 {
            let [a, b] = d.up((2.0 * PI * f * n as f64).sin());
            let y = d.down(a, b);
            if n >= 200 {
                sum += y * y;
                count += 1;
            }
        }
        (2.0 * sum / count as f64).sqrt()
    }

    /// Level of the image a sine at `f` leaves in the doubled signal, relative to the sine.
    fn image(f: f64) -> f64 {
        let mut d = Doubler::new();
        let (mut re, mut im, mut n2) = (0.0, 0.0, 0);
        for n in 0..8000 {
            for y in d.up((2.0 * PI * f * n as f64).sin()) {
                // the image sits at (1 - f) of the input rate, i.e. (1 - f) / 2 of the doubled rate
                let ph = 2.0 * PI * (1.0 - f) / 2.0 * n2 as f64;
                re += y * ph.cos();
                im += y * ph.sin();
                n2 += 1;
            }
        }
        2.0 * (re * re + im * im).sqrt() / n2 as f64
    }

    #[test]
    fn doubler_is_transparent_in_band_and_rejects_images() {
        // 20 kHz is 0.104 of the 4x rate
        for f in [0.001, 0.05, 0.104] {
            let g = round_trip(f);
            println!("round trip at {f}: {:+.3} dB", 20.0 * g.log10());
            assert!((g - 1.0).abs() < 0.01);
        }
        let im = image(0.125);
        println!("image of the band edge: {:.1} dB", 20.0 * im.log10());
        assert!(im < 1e-3);
    }
}
