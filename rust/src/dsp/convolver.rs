//! Zero-latency direct-form FIR convolver for cabinet impulse responses, plus the built-in cabs.
//! IRs are truncated to MAX_LENGTH, which covers typical guitar cab IRs.

use super::filters::Biquad;
use std::f64::consts::PI;

pub const MAX_LENGTH: usize = 2048;
const LANES: usize = 8;

/// An IR ready to swap into the convolver: everything is allocated off the audio thread.
pub struct PreparedIr {
    rev: Vec<f32>,
    hist: Vec<f32>,
}

impl PreparedIr {
    pub fn new(ir: &[f32]) -> Self {
        let len = ir.len().clamp(1, MAX_LENGTH);
        // round up to the vector width so the dot product has no tail loop
        let padded = len.div_ceil(LANES) * LANES;
        let mut rev = vec![0.0f32; padded];
        for (i, &v) in ir.iter().take(len).enumerate() {
            let fade = if i + 64 > len { (len - i) as f32 / 64.0 } else { 1.0 }; // avoid truncation click
            rev[padded - 1 - i] = v * fade;
        }
        PreparedIr { rev, hist: vec![0.0; padded * 2] }
    }
}

pub struct Convolver {
    ir: PreparedIr,
    pos: usize,
}

impl Default for Convolver {
    fn default() -> Self {
        Convolver { ir: PreparedIr::new(&[1.0]), pos: 0 }
    }
}

impl Convolver {
    /// Swaps in a new IR without allocating; returns the old one so it can be dropped elsewhere.
    pub fn swap(&mut self, ir: PreparedIr) -> PreparedIr {
        self.pos = 0;
        std::mem::replace(&mut self.ir, ir)
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        let n = self.ir.rev.len();
        for s in buf.iter_mut() {
            self.pos = (self.pos + 1) % n;
            self.ir.hist[self.pos] = *s;
            self.ir.hist[self.pos + n] = *s;
            let h = &self.ir.hist[self.pos + 1..self.pos + 1 + n];
            let mut acc = [0.0f32; LANES];
            for (hc, kc) in h.chunks_exact(LANES).zip(self.ir.rev.chunks_exact(LANES)) {
                for k in 0..LANES {
                    acc[k] += hc[k] * kc[k];
                }
            }
            *s = acc.iter().sum();
        }
    }
}

/// Scales an IR so its loudest band (100 Hz–6 kHz) sits at unity gain.
pub fn normalize(ir: &mut [f32], fs: f64) {
    let mut peak: f64 = 1e-9;
    let n = ir.len().min(MAX_LENGTH);
    let mut f = 100.0;
    while f < 6000.0 {
        let (mut re, mut im) = (0.0, 0.0);
        let w = 2.0 * PI * f / fs;
        for (i, &v) in ir[..n].iter().enumerate() {
            re += v as f64 * (w * i as f64).cos();
            im -= v as f64 * (w * i as f64).sin();
        }
        peak = peak.max((re * re + im * im).sqrt());
        f *= 1.06;
    }
    let g = (1.0 / peak) as f32;
    for v in ir.iter_mut() {
        *v *= g;
    }
}

/// Built-in cabs: an impulse pushed through a speaker-shaped EQ plus early reflections.
pub fn synthetic_cab(fs: f64, model: usize) -> Vec<f32> {
    let len = (fs * 0.03) as usize;
    let mut ir = vec![0.0f32; len];
    let mut f = [Biquad::default(); 8];
    match model {
        1 => {
            // 2x12 alnico: looser lows, smooth mids, chimey top
            f[0].high_pass(fs, 85.0, 0.7);
            f[1].peak(fs, 105.0, 1.2, 3.0);
            f[2].peak(fs, 600.0, 0.9, -1.5);
            f[3].peak(fs, 1600.0, 1.5, 1.0);
            f[4].peak(fs, 3200.0, 1.4, 4.5);
            f[5].peak(fs, 5200.0, 3.0, 1.5);
            f[6].low_pass(fs, 6600.0, 0.75);
            f[7].low_pass(fs, 8000.0, 0.6);
        }
        2 => {
            // 1x12 open back: lighter bass, airy, upper-mid bark
            f[0].high_pass(fs, 100.0, 0.6);
            f[1].peak(fs, 130.0, 1.0, 2.0);
            f[2].peak(fs, 480.0, 1.2, -2.5);
            f[3].peak(fs, 1100.0, 1.4, 2.0);
            f[4].peak(fs, 2200.0, 1.6, 3.5);
            f[5].peak(fs, 4300.0, 2.5, 2.0);
            f[6].low_pass(fs, 6000.0, 0.7);
            f[7].low_pass(fs, 7500.0, 0.6);
        }
        _ => {
            // 4x12 V30: tight thump, mid scoop, aggressive upper mids
            f[0].high_pass(fs, 70.0, 0.8);
            f[1].peak(fs, 115.0, 1.4, 5.0);
            f[2].peak(fs, 420.0, 1.0, -4.0);
            f[3].peak(fs, 1300.0, 2.0, 1.5);
            f[4].peak(fs, 2600.0, 1.6, 5.0);
            f[5].peak(fs, 4100.0, 3.0, 2.5);
            f[6].low_pass(fs, 5200.0, 0.75);
            f[7].low_pass(fs, 6500.0, 0.6);
        }
    }
    let (d1, d2) = ((fs * 0.00055) as usize, (fs * 0.0013) as usize);
    for (i, out) in ir.iter_mut().enumerate() {
        let mut x = if i == 0 { 1.0 } else { 0.0 };
        if i == d1 {
            x += 0.22; // cabinet early reflections
        }
        if i == d2 {
            x -= 0.12;
        }
        for b in f.iter_mut() {
            x = b.process(x);
        }
        *out = x as f32;
    }
    normalize(&mut ir, fs);
    ir
}

/// Linear-interpolation resampler for user IRs.
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let n = (x.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let t = i as f64 * step;
            let j = t as usize;
            let f = t - j as f64;
            let a = x[j.min(x.len() - 1)];
            let b = x[(j + 1).min(x.len() - 1)];
            (a as f64 + (b - a) as f64 * f) as f32
        })
        .collect()
}

/// Builds the IR for a cab model: 0..2 built-in, 3 = the user IR (falls back to model 0).
pub fn build_cab(fs: f64, model: usize, user: Option<(&[f32], u32)>) -> PreparedIr {
    let ir = match (model, user) {
        (3, Some((data, rate))) => {
            let mut ir = resample(data, rate, fs as u32);
            normalize(&mut ir, fs);
            ir
        }
        _ => synthetic_cab(fs, model.min(2)),
    };
    PreparedIr::new(&ir)
}
