//! Cabinet simulation: a zero-latency partitioned convolver that runs several IRs at once into a
//! stereo pair, the built-in cabs, and IR utilities (normalizing, resampling).
//!
//! The convolver splits each IR into blocks of `B` taps. The first block runs directly, sample by
//! sample, so there is no latency; the rest runs in the frequency domain once per block, sharing one
//! FFT of the (mono) input across every IR. Each IR has a gain into L and R, smoothed per block, which
//! is how the built-in cabs crossfade between mic positions.
//!
//! The built-in cabs are designed in the frequency domain: the original voicing curve, the mic's EQ at
//! that position, and dozens of narrow cone-breakup peaks and dips whose depth and frequency shift with
//! the mic position. Each response is made minimum-phase, then gets filtered cabinet reflections and a
//! short diffuse tail. Every cab is two speakers (L/R) at four mic positions, each blended with a
//! ribbon mic, so a cab is 10 IRs.

use super::filters::Biquad;
use std::f64::consts::PI;

/// Longest IR kept, in samples (user IRs are truncated here).
pub const MAX_LENGTH: usize = 16384;
/// Taps run directly per sample; also the FFT partition size (FFT size is 2·B).
const B: usize = 64;
const BINS: usize = B + 1;
const LANES: usize = 8;
/// Mic positions each built-in cab is designed at, evenly spaced from cap (0) to cone (1).
const POSITIONS: usize = 4;
/// Share of the ribbon mic in the built-in cabs' dual-mic blend.
const RIBBON_MIX: f32 = 0.3;
/// Length of the built-in cab IRs, in seconds.
const BUILTIN_SECONDS: f64 = 0.045;

// ------------------------------------------------------------------ FFT

/// In-place radix-2 complex FFT.
pub struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<usize>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let rev = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let cos = (0..n / 2).map(|k| (2.0 * PI * k as f64 / n as f64).cos()).collect();
        let sin = (0..n / 2).map(|k| (2.0 * PI * k as f64 / n as f64).sin()).collect();
        Fft { n, cos, sin, rev }
    }

    /// Forward transform uses e^-iwt; the inverse is unscaled (divide by n yourself).
    pub fn run(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { 1.0 } else { -1.0 };
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], sign * self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

// ------------------------------------------------------------------ convolver

/// A set of IRs ready to swap into the convolver: everything is allocated off the audio thread.
pub struct PreparedIr {
    n_ir: usize,
    /// FFT partitions after the direct head (0 for IRs no longer than `B`).
    parts: usize,
    /// First `B` taps of each IR.
    head: Vec<f32>,
    /// Spectra of each IR's later partitions: `[ir][part][bin]`.
    h_re: Vec<f32>,
    h_im: Vec<f32>,
    /// Gain of each IR into L and R: what the mic position asks for, and the smoothed value in use.
    target: Vec<[f32; 2]>,
    gain: Vec<[f32; 2]>,
    /// True for the built-in cabs (mic position crossfades IRs); false for one IR on both sides.
    positional: bool,
    last_pos: f32,
    smooth: f32,
    snap: bool,
    // running state
    fft: Fft,
    hist: Vec<f32>,
    hp: usize,
    kernel: [Vec<f32>; 2],
    x_re: Vec<f32>,
    x_im: Vec<f32>,
    x_pos: usize,
    frame: Vec<f64>,
    tail: [Vec<f32>; 2],
    pos: usize,
    f_re: Vec<f64>,
    f_im: Vec<f64>,
    acc: [(Vec<f64>, Vec<f64>); 2],
    t_re: Vec<f64>,
    t_im: Vec<f64>,
}

impl PreparedIr {
    /// One IR, the same on both sides.
    pub fn single(fs: f64, ir: &[f32]) -> Self {
        Self::build(fs, &[ir.to_vec()], vec![[1.0, 1.0]], false)
    }

    /// A built-in cab: `POSITIONS` × (L, R) dynamic-mic IRs, then the ribbon mic's (L, R).
    fn positional(fs: f64, irs: Vec<Vec<f32>>) -> Self {
        assert_eq!(irs.len(), POSITIONS * 2 + 2);
        let mut p = Self::build(fs, &irs, vec![[0.0; 2]; irs.len()], true);
        p.last_pos = -1.0;
        p.set_mic(0.3);
        p
    }

    fn build(fs: f64, irs: &[Vec<f32>], gains: Vec<[f32; 2]>, positional: bool) -> Self {
        let n_ir = irs.len();
        let len = irs.iter().map(|v| v.len()).max().unwrap_or(1).clamp(1, MAX_LENGTH);
        let parts = len.div_ceil(B) - 1;
        let fft = Fft::new(2 * B);
        let mut head = vec![0.0f32; n_ir * B];
        let (mut h_re, mut h_im) = (vec![0.0f32; n_ir * parts * BINS], vec![0.0f32; n_ir * parts * BINS]);
        let (mut re, mut im) = (vec![0.0f64; 2 * B], vec![0.0f64; 2 * B]);
        let fade = (len / 8).clamp(1, 256);
        for (i, ir) in irs.iter().enumerate() {
            // fade the truncated end so it can't click
            let tap = |k: usize| -> f64 {
                let v = *ir.get(k).unwrap_or(&0.0) as f64;
                if k >= len {
                    0.0
                } else if k + fade > len {
                    v * 0.5 * (1.0 - (PI * (len - k) as f64 / fade as f64).cos())
                } else {
                    v
                }
            };
            for k in 0..B {
                head[i * B + k] = tap(k) as f32;
            }
            for p in 0..parts {
                for k in 0..2 * B {
                    re[k] = if k < B { tap((p + 1) * B + k) } else { 0.0 };
                    im[k] = 0.0;
                }
                fft.run(&mut re, &mut im, false);
                let o = (i * parts + p) * BINS;
                for k in 0..BINS {
                    h_re[o + k] = re[k] as f32;
                    h_im[o + k] = im[k] as f32;
                }
            }
        }
        let mut ir = PreparedIr {
            n_ir,
            parts,
            head,
            h_re,
            h_im,
            target: gains.clone(),
            gain: gains,
            positional,
            last_pos: 0.0,
            smooth: (1.0 - (-(B as f64) / (fs.max(1.0) * 0.03)).exp()) as f32,
            snap: true,
            fft,
            hist: vec![0.0; 2 * B],
            hp: 0,
            kernel: [vec![0.0; B], vec![0.0; B]],
            x_re: vec![0.0; parts.max(1) * BINS],
            x_im: vec![0.0; parts.max(1) * BINS],
            x_pos: 0,
            frame: vec![0.0; 2 * B],
            tail: [vec![0.0; B], vec![0.0; B]],
            pos: 0,
            f_re: re,
            f_im: im,
            acc: [(vec![0.0; BINS], vec![0.0; BINS]), (vec![0.0; BINS], vec![0.0; BINS])],
            t_re: vec![0.0; BINS],
            t_im: vec![0.0; BINS],
        };
        ir.update_kernels();
        ir
    }

    /// Points the built-in cab's mic at `pos` (0 = cap .. 1 = cone). No effect on a single IR.
    fn set_mic(&mut self, pos: f32) {
        if !self.positional || pos == self.last_pos {
            return;
        }
        self.last_pos = pos;
        let t = pos.clamp(0.0, 1.0) * (POSITIONS - 1) as f32;
        let k0 = (t as usize).min(POSITIONS - 2);
        let f = t - k0 as f32;
        let dynamic = 1.0 - RIBBON_MIX;
        for k in 0..POSITIONS {
            let w = dynamic * (if k == k0 { 1.0 - f } else if k == k0 + 1 { f } else { 0.0 });
            self.target[2 * k] = [w, 0.0];
            self.target[2 * k + 1] = [0.0, w];
        }
        self.target[2 * POSITIONS] = [RIBBON_MIX, 0.0];
        self.target[2 * POSITIONS + 1] = [0.0, RIBBON_MIX];
        if self.snap {
            self.gain.copy_from_slice(&self.target);
            self.update_kernels();
        }
    }

    /// Blends the direct-form heads with the current gains (reversed, for the dot product).
    fn update_kernels(&mut self) {
        for (c, kern) in self.kernel.iter_mut().enumerate() {
            kern.fill(0.0);
            for (i, g) in self.gain.iter().enumerate() {
                if g[c] != 0.0 {
                    for k in 0..B {
                        kern[B - 1 - k] += g[c] * self.head[i * B + k];
                    }
                }
            }
        }
    }

    /// A block of input is complete: compute the next block's contribution of every later partition.
    fn end_block(&mut self) {
        self.snap = false;
        for (g, t) in self.gain.iter_mut().zip(&self.target) {
            for c in 0..2 {
                g[c] += self.smooth * (t[c] - g[c]);
                if (g[c] - t[c]).abs() < 1e-5 {
                    g[c] = t[c];
                }
            }
        }
        self.update_kernels();
        let n = 2 * B;
        if self.parts > 0 {
            // spectrum of [previous block, this block] (overlap-save)
            self.f_re.copy_from_slice(&self.frame);
            self.f_im.fill(0.0);
            self.fft.run(&mut self.f_re, &mut self.f_im, false);
            self.x_pos = (self.x_pos + 1) % self.parts;
            let o = self.x_pos * BINS;
            for k in 0..BINS {
                self.x_re[o + k] = self.f_re[k] as f32;
                self.x_im[o + k] = self.f_im[k] as f32;
            }
            for (a, b) in self.acc.iter_mut() {
                a.fill(0.0);
                b.fill(0.0);
            }
            for i in 0..self.n_ir {
                let g = self.gain[i];
                if g[0] == 0.0 && g[1] == 0.0 {
                    continue;
                }
                self.t_re.fill(0.0);
                self.t_im.fill(0.0);
                for p in 0..self.parts {
                    // partition p+1 meets the input spectrum from p blocks before the newest
                    let xo = ((self.x_pos + self.parts - p) % self.parts) * BINS;
                    let ho = (i * self.parts + p) * BINS;
                    let (hr, hi) = (&self.h_re[ho..ho + BINS], &self.h_im[ho..ho + BINS]);
                    let (xr, xi) = (&self.x_re[xo..xo + BINS], &self.x_im[xo..xo + BINS]);
                    for k in 0..BINS {
                        self.t_re[k] += (hr[k] * xr[k] - hi[k] * xi[k]) as f64;
                        self.t_im[k] += (hr[k] * xi[k] + hi[k] * xr[k]) as f64;
                    }
                }
                for (c, (ar, ai)) in self.acc.iter_mut().enumerate() {
                    if g[c] != 0.0 {
                        for k in 0..BINS {
                            ar[k] += g[c] as f64 * self.t_re[k];
                            ai[k] += g[c] as f64 * self.t_im[k];
                        }
                    }
                }
            }
            for c in 0..2 {
                let (ar, ai) = &self.acc[c];
                for k in 0..BINS {
                    self.f_re[k] = ar[k];
                    self.f_im[k] = ai[k];
                }
                for k in 1..B {
                    self.f_re[n - k] = ar[k];
                    self.f_im[n - k] = -ai[k];
                }
                self.fft.run(&mut self.f_re, &mut self.f_im, true);
                for k in 0..B {
                    self.tail[c][k] = (self.f_re[B + k] / n as f64) as f32;
                }
            }
        }
        self.frame.copy_within(B.., 0);
    }
}

pub struct Convolver {
    ir: PreparedIr,
}

impl Default for Convolver {
    fn default() -> Self {
        Convolver { ir: PreparedIr::single(48000.0, &[1.0]) }
    }
}

impl Convolver {
    /// Swaps in a new IR set without allocating; returns the old one so it can be dropped elsewhere.
    pub fn swap(&mut self, ir: PreparedIr) -> PreparedIr {
        std::mem::replace(&mut self.ir, ir)
    }

    /// True when the mic position is part of the IRs (built-in cabs) rather than an EQ after them.
    pub fn positional(&self) -> bool {
        self.ir.positional
    }

    /// Mic position for the built-in cabs (0 = cap .. 1 = cone), crossfaded smoothly.
    pub fn set_mic(&mut self, pos: f32) {
        self.ir.set_mic(pos);
    }

    /// Mono in, stereo out.
    pub fn process(&mut self, input: &[f32], l: &mut [f32], r: &mut [f32]) {
        let ir = &mut self.ir;
        for i in 0..input.len() {
            let x = input[i];
            ir.hp = (ir.hp + 1) % B;
            ir.hist[ir.hp] = x;
            ir.hist[ir.hp + B] = x;
            let h = &ir.hist[ir.hp + 1..ir.hp + 1 + B];
            let (mut al, mut ar) = ([0.0f32; LANES], [0.0f32; LANES]);
            for ((hc, kl), kr) in h.chunks_exact(LANES).zip(ir.kernel[0].chunks_exact(LANES)).zip(ir.kernel[1].chunks_exact(LANES)) {
                for k in 0..LANES {
                    al[k] += hc[k] * kl[k];
                    ar[k] += hc[k] * kr[k];
                }
            }
            l[i] = al.iter().sum::<f32>() + ir.tail[0][ir.pos];
            r[i] = ar.iter().sum::<f32>() + ir.tail[1][ir.pos];
            ir.frame[B + ir.pos] = x as f64;
            ir.pos += 1;
            if ir.pos == B {
                ir.pos = 0;
                ir.end_block();
            }
        }
    }
}

// ------------------------------------------------------------------ IR utilities

/// Peak gain and mean power gain of an IR between 100 Hz and 6 kHz (log-spaced).
fn band_stats(ir: &[f32], fs: f64) -> (f64, f64) {
    let (mut peak, mut sum, mut count): (f64, f64, f64) = (1e-9, 0.0, 0.0);
    let n = ir.len().min(MAX_LENGTH);
    let mut f = 100.0;
    while f < 6000.0 {
        let (mut re, mut im) = (0.0, 0.0);
        let w = 2.0 * PI * f / fs;
        for (i, &v) in ir[..n].iter().enumerate() {
            re += v as f64 * (w * i as f64).cos();
            im -= v as f64 * (w * i as f64).sin();
        }
        let p = re * re + im * im;
        peak = peak.max(p.sqrt());
        (sum, count) = (sum + p, count + 1.0);
        f *= 1.06;
    }
    (peak, sum / count)
}

fn band_peak(ir: &[f32], fs: f64) -> f64 {
    band_stats(ir, fs).0
}

/// Scales an IR so its loudest band (100 Hz–6 kHz) sits at unity gain.
pub fn normalize(ir: &mut [f32], fs: f64) {
    let g = (1.0 / band_peak(ir, fs)) as f32;
    for v in ir.iter_mut() {
        *v *= g;
    }
}

/// Windowed-sinc resampler (Blackman window, 24 zero crossings each side, anti-aliased when
/// downsampling).
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() || from == 0 || to == 0 {
        return x.to_vec();
    }
    let n = (x.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    let cut = (to as f64 / from as f64).min(1.0) * 0.96;
    let hw = 24.0 / cut;
    let last = x.len() as isize - 1;
    (0..n)
        .map(|i| {
            let t = i as f64 * step;
            let (a, b) = ((t - hw).ceil() as isize, (t + hw).floor() as isize);
            let mut acc = 0.0;
            for k in a.max(0)..=b.min(last) {
                let d = t - k as f64;
                let u = d / hw;
                let win = 0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos();
                let s = if d.abs() < 1e-9 { 1.0 } else { (PI * cut * d).sin() / (PI * cut * d) };
                acc += x[k as usize] as f64 * cut * s * win;
            }
            acc as f32
        })
        .collect()
}

// ------------------------------------------------------------------ built-in cabs

/// Small deterministic RNG (splitmix64) so the built-in cabs sound the same every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as f64 / u64::MAX as f64
    }
    /// Uniform in -1..1.
    fn bi(&mut self) -> f64 {
        self.next() * 2.0 - 1.0
    }
}

/// Each cab's voicing: the tonal balance of the speaker in its cabinet (this was the whole cab
/// before; the detail below is layered on top so presets keep their balance).
fn voicing(fs: f64, model: usize) -> [Biquad; 8] {
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
    f
}

/// One cone-breakup resonance (or dip, when `amp` is negative).
struct Mode {
    f: f64,
    q: f64,
    amp: f64,
    /// How fast the mode changes as the mic moves across the cone, and where it starts.
    k: f64,
    ph: f64,
}

/// The cone's breakup modes as heard by one speaker. `jitter` is how much this speaker differs from
/// the cab's other speakers (0 = identical).
fn breakup_modes(fs: f64, model: usize, speaker: usize, jitter: f64) -> Vec<Mode> {
    let mut base = Rng(0x5EED_CAB0 + model as u64 * 7919);
    let mut spk = Rng(0xC0FF_EE00 ^ (model as u64 * 131 + speaker as u64 * 977 + 1));
    let f_lo = 1100.0;
    let f_hi = 9500.0f64.min(0.42 * fs);
    let m = 44;
    (0..m)
        .map(|i| {
            let t = (i as f64 + 0.15 + 0.7 * base.next()) / m as f64;
            let f = f_lo * (f_hi / f_lo).powf(t);
            let q = 5.0 + 13.0 * base.next();
            let env = 2.0 + 5.0 * (t * t * (3.0 - 2.0 * t)); // breakup grows toward the top
            let mut amp = env * (0.35 + 0.65 * base.next());
            if base.next() < 0.4 {
                amp *= -1.3; // cancellations dip deeper than resonances peak
            }
            let k = 0.6 + 2.4 * t;
            let ph = 2.0 * base.next();
            Mode {
                f: f * (1.0 + jitter * 0.08 * spk.bi()),
                q,
                amp: amp * (1.0 + jitter * 0.6 * spk.bi()),
                k,
                ph: ph + jitter * 0.6 * spk.bi(),
            }
        })
        .collect()
}

/// The filters for one mic on one speaker. `pos` is the mic position (0 = cap .. 1 = cone).
fn mic_filters(fs: f64, model: usize, speaker: usize, pos: f64, ribbon: bool) -> Vec<Biquad> {
    let jitter = if model == 2 { 0.45 } else { 1.0 }; // the 1x12's "two speakers" are one speaker, two mics
    let mut v: Vec<Biquad> = voicing(fs, model).to_vec();
    let mut add = |set: &dyn Fn(&mut Biquad)| {
        let mut b = Biquad::default();
        set(&mut b);
        v.push(b);
    };
    // speaker-to-speaker variation in the low resonance and the midrange
    let mut spk = Rng(0xA11C_E000 ^ (model as u64 * 31 + speaker as u64 * 7 + 3));
    let (fr, gr) = (110.0 * (1.0 + 0.06 * jitter * spk.bi()), 1.0 * jitter * spk.bi());
    let (fm, gm) = (700.0 + 700.0 * spk.next(), 1.5 * jitter * spk.bi());
    add(&|b| b.peak(fs, fr, 2.0, gr));
    add(&|b| b.peak(fs, fm, 1.5, gm));
    let (pos, scale) = if ribbon {
        // a ribbon a little further back: smooth top, fuller lows, sees the cone more evenly
        add(&|b| b.low_shelf(fs, 160.0, 2.0));
        add(&|b| b.high_shelf(fs, 3500.0, -4.5));
        add(&|b| b.low_pass(fs, 8500.0, 0.6));
        (0.5, 0.55)
    } else {
        // the dynamic mic's EQ at this position, as before: cap = bright, cone = dark
        add(&|b| b.high_shelf(fs, 2500.0, 2.5 - 10.0 * pos));
        add(&|b| b.low_pass(fs, (16000.0 - 9000.0 * pos).min(0.45 * fs), 0.7));
        (pos, 1.0)
    };
    for m in breakup_modes(fs, model, speaker, jitter) {
        let a = PI * (m.k * pos + m.ph);
        let f = (m.f * (1.0 + 0.025 * a.sin())).min(0.45 * fs);
        let g = scale * m.amp * (0.55 + 0.45 * a.cos());
        add(&|b| b.peak(fs, f, m.q, g));
    }
    v
}

/// The minimum-phase impulse response with the magnitude of `filters`, `len` samples long.
fn min_phase_ir(fs: f64, filters: &[Biquad], len: usize) -> Vec<f64> {
    let n = ((fs * 0.25) as usize).next_power_of_two().max(8192);
    let fft = Fft::new(n);
    let (mut re, mut im) = (vec![0.0f64; n], vec![0.0f64; n]);
    // log magnitude (natural log), mirrored
    for k in 0..=n / 2 {
        let w = 2.0 * PI * k as f64 / n as f64;
        let z = [w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin()];
        let p: f64 = filters.iter().map(|b| b.power_at(z)).product();
        re[k] = 0.5 * p.max(1e-12).ln();
        if k > 0 && k < n / 2 {
            re[n - k] = re[k];
        }
    }
    // real cepstrum, folded onto positive quefrencies
    fft.run(&mut re, &mut im, true);
    for k in 0..n {
        re[k] /= n as f64;
        im[k] = 0.0;
    }
    for k in 1..n / 2 {
        re[k] *= 2.0;
    }
    for v in re.iter_mut().skip(n / 2 + 1) {
        *v = 0.0;
    }
    fft.run(&mut re, &mut im, false);
    for k in 0..n {
        let m = re[k].exp();
        (re[k], im[k]) = (m * im[k].cos(), m * im[k].sin());
    }
    fft.run(&mut re, &mut im, true);
    re.truncate(len);
    re.iter_mut().for_each(|v| *v /= n as f64);
    re
}

/// Cabinet reflections (dulled by the box) and a short diffuse tail, seeded per IR.
fn add_cabinet(fs: f64, model: usize, h: &[f64], seed: u64) -> Vec<f32> {
    let len = h.len();
    let mut out = h.to_vec();
    let mut refl = |delay_s: f64, gain: f64, lp_hz: f64| {
        let d = (fs * delay_s) as usize;
        let mut lp = Biquad::default();
        lp.low_pass(fs, lp_hz, 0.6);
        for i in 0..len.saturating_sub(d) {
            out[i + d] += gain * lp.process(h[i]);
        }
    };
    refl(0.00055, 0.18, 3500.0);
    refl(0.0013, -0.10, 2500.0);
    if model == 2 {
        refl(0.0024, 0.14, 1800.0); // open back: the wall behind the cab
    }
    let peak = h.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let mut rng = Rng(seed);
    let mut lp = Biquad::default();
    lp.low_pass(fs, 3500.0, 0.6);
    let (t0, tau) = (fs * 0.0006, fs * 0.005);
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64;
        let env = if t < t0 { t / t0 } else { (-(t - t0) / tau).exp() };
        *o += lp.process(0.03 * peak * env * rng.bi());
    }
    out.iter().map(|&v| v as f32).collect()
}

/// Mean power (100 Hz–6 kHz) of the old cab: voicing plus the mic EQ at the default position,
/// normalized to a unity peak.
fn reference_power(fs: f64, model: usize) -> f64 {
    let mut v = voicing(fs, model).to_vec();
    let (mut a, mut b) = (Biquad::default(), Biquad::default());
    a.high_shelf(fs, 2500.0, -0.5);
    b.low_pass(fs, 13300.0, 0.7);
    v.extend([a, b]);
    let (mut peak, mut sum, mut count): (f64, f64, f64) = (1e-9, 0.0, 0.0);
    let mut f = 100.0;
    while f < 6000.0 {
        let w = 2.0 * PI * f / fs;
        let p: f64 = v.iter().map(|q| q.power_at([w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin()])).product();
        peak = peak.max(p);
        (sum, count) = (sum + p, count + 1.0);
        f *= 1.06;
    }
    sum / count / peak
}

/// The 10 IRs of a built-in cab, in the order `PreparedIr::positional` expects.
fn builtin_cab(fs: f64, model: usize) -> Vec<Vec<f32>> {
    let len = (fs * BUILTIN_SECONDS) as usize;
    let one = |speaker: usize, pos: f64, ribbon: bool, seed: u64| {
        let h = min_phase_ir(fs, &mic_filters(fs, model, speaker, pos, ribbon), len);
        add_cabinet(fs, model, &h, seed ^ (model as u64 * 0x1000 + speaker as u64 * 0x100))
    };
    let mut irs = Vec::with_capacity(POSITIONS * 2 + 2);
    for k in 0..POSITIONS {
        let pos = k as f64 / (POSITIONS - 1) as f64;
        for s in 0..2 {
            irs.push(one(s, pos, false, 0xD1A0 + k as u64));
        }
    }
    for s in 0..2 {
        irs.push(one(s, 0.5, true, 0x41BB));
    }
    // one gain for the whole cab: the default mic position's blend gets the average loudness the
    // old (voicing-only) cab had, so presets keep their level
    let mix: Vec<f32> = irs[2].iter().zip(&irs[2 * POSITIONS]).map(|(d, r)| d * (1.0 - RIBBON_MIX) + r * RIBBON_MIX).collect();
    // (the trim takes back what the breakup peaks add on real playing, measured on the presets)
    const TRIM: [f64; 3] = [0.87, 0.79, 0.785];
    let g = (TRIM[model] * (reference_power(fs, model) / band_stats(&mix, fs).1).sqrt()) as f32;
    for ir in irs.iter_mut() {
        ir.iter_mut().for_each(|v| *v *= g);
    }
    irs
}

/// Builds the IR set for a cab model: 0..2 built-in, 3 = the user IR (falls back to model 0).
pub fn build_cab(fs: f64, model: usize, user: Option<(&[f32], u32)>) -> PreparedIr {
    match (model, user) {
        (3, Some((data, rate))) => {
            let mut ir = resample(data, rate, fs as u32);
            ir.truncate(MAX_LENGTH);
            normalize(&mut ir, fs);
            PreparedIr::single(fs, &ir)
        }
        _ => PreparedIr::positional(fs, builtin_cab(fs, model.min(2))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
        (0..x.len()).map(|n| (0..h.len().min(n + 1)).map(|k| h[k] as f64 * x[n - k] as f64).sum::<f64>() as f32).collect()
    }

    #[test]
    fn partitioned_matches_direct_convolution() {
        let mut rng = Rng(7);
        let h: Vec<f32> = (0..700).map(|i| (rng.bi() * (-(i as f64) / 150.0).exp()) as f32).collect();
        let x: Vec<f32> = (0..3000).map(|_| rng.bi() as f32).collect();
        // no end fade on this IR: pad it so the fade only touches zeros
        let mut hp = h.clone();
        hp.resize(1100, 0.0);
        let mut c = Convolver::default();
        c.swap(PreparedIr::single(48000.0, &hp));
        let (mut l, mut r) = (vec![0.0; x.len()], vec![0.0; x.len()]);
        let mut off = 0;
        for chunk in [1usize, 63, 64, 65, 200, 7].iter().cycle() {
            if off >= x.len() {
                break;
            }
            let n = (*chunk).min(x.len() - off);
            c.process(&x[off..off + n], &mut l[off..off + n], &mut r[off..off + n]);
            off += n;
        }
        let want = direct(&x, &h);
        for i in 0..x.len() {
            assert!((l[i] - want[i]).abs() < 1e-4, "sample {i}: {} vs {}", l[i], want[i]);
            assert_eq!(l[i], r[i]);
        }
    }

    #[test]
    fn builtin_cabs_are_stereo_and_sane() {
        for model in 0..3 {
            let mut c = Convolver::default();
            c.swap(build_cab(48000.0, model, None));
            assert!(c.positional());
            let mut x = vec![0.0f32; 4800];
            x[0] = 1.0;
            for pos in [0.0f32, 0.3, 1.0] {
                c.set_mic(pos);
                let (mut l, mut r) = (vec![0.0; x.len()], vec![0.0; x.len()]);
                c.process(&x, &mut l, &mut r);
                let pk = band_peak(&l, 48000.0);
                assert!(pk > 0.3 && pk < 3.0, "model {model} pos {pos}: peak {pk}");
                let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
                assert!(diff > 1e-3, "L and R should differ");
                assert!(l.iter().chain(&r).all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn resampler_keeps_a_sine() {
        let x: Vec<f32> = (0..4410).map(|i| (2.0 * PI * 1000.0 * i as f64 / 44100.0).sin() as f32).collect();
        let y = resample(&x, 44100, 48000);
        for i in 200..y.len() - 200 {
            let want = (2.0 * PI * 1000.0 * i as f64 / 48000.0).sin() as f32;
            assert!((y[i] - want).abs() < 2e-3, "sample {i}");
        }
    }
}

