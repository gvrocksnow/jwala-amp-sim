//! The amp's ACOUSTIC channel: turns a magnetic-pickup DI into a miked acoustic guitar.
//! Pickup correction → "sparkle" exciter → modal body IR → air/top resonances → EQ.
//! The body's spectral envelope was tone-matched (1/3-octave LTAS, octave-smoothed below
//! 250 Hz where the notes played dominate) to a steel-string dreadnought recording (Ibanez
//! AAD300CE), played from a real magnetic-pickup DI.
//!
//! Knob meanings on this channel: GAIN = BODY (how much of the body's ringing tail
//! is heard; low = drier, piezo-like), BASS/MIDDLE/TREBLE = EQ (centre = matched),
//! PRESENCE = AIR (sparkle + top shelf), DEPTH = RESO (air-cavity/top-plate
//! resonances), MASTER = LEVEL.

use super::convolver::normalize;
use super::filters::Biquad;
use super::state::AmpState;
use std::f64::consts::PI;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Body spectral envelope in dB at the 1/3-octave centres 20 Hz · 2^(i/3), i = 0..30.
const BODY_DB: [f64; 31] = [
    26.0, 26.0, 26.0, 26.0, 26.0, 26.0, 26.0, 19.1, 15.8, -4.8, 11.9, 21.2, 14.7, 6.6, 18.7, -10.5, -8.4, -1.9, 2.4, -4.5,
    -0.7, -0.4, 1.2, -11.1, -8.2, -11.8, -9.6, -10.5, -4.9, 0.4, 0.4,
];

/// Loudness make-up so the channel sits near the clean channel's level.
const MAKEUP: f64 = 29.6;

const MAX_TAPS: usize = 2048;
const LANES: usize = 8;

/// xorshift32; the body IR must come out identical in the C# version.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f64 / 16_777_216.0
    }
}

/// Envelope gain (dB) at `f`, interpolated on a log-frequency axis, clamped at the ends.
fn body_db(f: f64) -> f64 {
    let pos = 3.0 * (f / 20.0).log2();
    if pos <= 0.0 {
        return BODY_DB[0];
    }
    let i = pos.floor() as usize;
    if i >= BODY_DB.len() - 1 {
        return BODY_DB[BODY_DB.len() - 1];
    }
    let t = pos - i as f64;
    BODY_DB[i] + (BODY_DB[i + 1] - BODY_DB[i]) * t
}

/// The body's impulse response: a sum of exponentially decaying modes, sparse and
/// long-ringing in the lows, dense and short in the highs. Each mode's amplitude is
/// set so the power spectrum follows `BODY_DB` whatever the mode density and decay.
pub fn body_ir(fs: f64) -> Vec<f32> {
    let n = MAX_TAPS.min((fs * 0.045) as usize);
    let mut ir = vec![0.0f64; n];
    let mut rng = Rng(0x2F6B_9A1D);
    let top = 18000.0f64.min(0.45 * fs);
    let mut f = 70.0;
    while f < top {
        let spacing = (0.30 * (f / 70.0).powf(-0.75)).max(0.018);
        let df = f * spacing;
        let fk = f * (1.0 + (rng.next() - 0.5) * spacing);
        let t60 = (if fk < 250.0 { 0.040 } else { 1.0f64 }).min(0.06 * (fk / 250.0).powf(-0.55));
        let tau = t60 / 6.91;
        let amp = (df / tau).sqrt() * db(body_db(fk)) * (0.55 + 0.9 * rng.next());
        let sign = if rng.next() < 0.5 { 1.0 } else { -1.0 };
        // decaying sine by recursion: rotate a phasor and shrink it every sample
        let (w, d) = (2.0 * PI * fk / fs, (-1.0 / (fs * tau)).exp());
        let (c, s) = (w.cos() * d, w.sin() * d);
        let (mut re, mut im) = (sign * amp, 0.0);
        for v in ir.iter_mut() {
            *v += im;
            let r2 = re * c - im * s;
            im = re * s + im * c;
            re = r2;
        }
        f += df;
    }
    let mut out: Vec<f32> = ir.iter().map(|&v| v as f32).collect();
    normalize(&mut out, fs);
    out
}

/// Direct-form FIR that returns the body IR's first 2.5 ms ("head") and the rest
/// ("tail") separately, so BODY can scale the ringing without changing the attack.
struct BodyFir {
    /// Reversed taps; `head` holds only the last (earliest) taps of the reversed IR.
    head: Vec<f32>,
    tail: Vec<f32>,
    hist: Vec<f32>,
    pos: usize,
}

impl BodyFir {
    fn new(ir: &[f32], fs: f64) -> Self {
        let len = ir.len().clamp(1, MAX_TAPS);
        let padded = len.div_ceil(LANES) * LANES;
        let head_len = ((fs * 0.0025) as usize + 1).div_ceil(LANES).min(padded / LANES) * LANES;
        let (mut head, mut tail) = (vec![0.0f32; head_len], vec![0.0f32; padded]);
        for (i, &v) in ir.iter().take(len).enumerate() {
            let fade = if i + 64 > len { (len - i) as f32 / 64.0 } else { 1.0 };
            // tail weight: 0 before 0.5 ms, raised cosine up to 1 at 2.5 ms
            let u = ((i as f64 / fs - 0.0005) / 0.002).clamp(0.0, 1.0);
            let w = (0.5 - 0.5 * (PI * u).cos()) as f32;
            if i < head_len {
                head[head_len - 1 - i] = v * fade * (1.0 - w);
            }
            tail[padded - 1 - i] = v * fade * w;
        }
        BodyFir { head, tail, hist: vec![0.0; padded * 2], pos: 0 }
    }

    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let n = self.tail.len();
        self.pos = (self.pos + 1) % n;
        self.hist[self.pos] = x;
        self.hist[self.pos + n] = x;
        let h = &self.hist[self.pos + 1..self.pos + 1 + n];
        let (mut a, mut b) = ([0.0f32; LANES], [0.0f32; LANES]);
        for (hc, kt) in h.chunks_exact(LANES).zip(self.tail.chunks_exact(LANES)) {
            for k in 0..LANES {
                b[k] += hc[k] * kt[k];
            }
        }
        for (hc, kh) in h[n - self.head.len()..].chunks_exact(LANES).zip(self.head.chunks_exact(LANES)) {
            for k in 0..LANES {
                a[k] += hc[k] * kh[k];
            }
        }
        (a.iter().sum(), b.iter().sum())
    }
}

/// Crossovers of the shaper's four bands. Each band is the residual of the ones below, so
/// the bands always sum back to the input.
const SPLIT_HZ: [f64; 3] = [400.0, 1500.0, 5000.0];

// The DECAY, ATTACK and DYNAMIC switches, tuned on note-onset statistics of the Ibanez
// recording (decay rate per band, envelope shape around each pick) with the same DI.

/// DECAY: the slowest each band may fade after a pick, dB/s (a real top soaks up the
/// string's energy, trebles fastest).
const DECAY_DB_S: [f64; 4] = [20.0, 17.0, 22.0, 34.0];
/// ATTACK: per band, gain at the pick (dB), fading out over `ATTACK_MS`: the pickup's
/// upper-mid pick spike tamed and the string zing raised.
const ATTACK_DB: [f64; 4] = [0.0, 0.0, -2.0, 3.0];
const ATTACK_MS: f64 = 20.0;
/// DYNAMIC: treble-shelf dB per dB that a pick is harder/softer than the recent picks, the
/// shelf's limit, and an offset that keeps the average tone where it was matched (harder
/// picks are also louder, so they would otherwise brighten the overall sound).
const DYN_DB_PER_DB: f64 = 0.5;
const DYN_MAX_DB: f64 = 6.0;
const DYN_OFFSET_DB: f64 = 1.0;

fn coef(fs: f64, ms: f64) -> f64 {
    1.0 - (-1000.0 / (fs * ms)).exp()
}

/// The acoustic channel's three switches, applied to the pickup-corrected string signal
/// before the body. DECAY caps each band's envelope so it can't fade slower than an
/// acoustic's; a pick detector drives ATTACK (per-band gains that fade out after each
/// pick) and DYNAMIC (a treble shelf set at each pick by how hard it was picked compared
/// with the recent picks).
struct Shaper {
    fs: f64,
    split: [Biquad; 3],
    env: [f64; 4],
    slow: [f64; 4],
    /// DECAY: the envelope a band may not exceed.
    cap: [f64; 4],
    gain: [f64; 4],
    env_a: f64,
    env_r: [f64; 4],
    slow_a: f64,
    gain_a: [f64; 4],
    decay_k: [f64; 4],
    /// Pick detector: fast and slow broadband envelopes, and the hold-off between picks.
    pick_fast: f64,
    pick_slow: f64,
    pick_fa: f64,
    pick_fr: f64,
    pick_sa: f64,
    hold: usize,
    /// ATTACK: 1 at a pick, decaying.
    since: f64,
    since_k: f64,
    /// DYNAMIC: the pick's peak so far, samples until it is judged, the running average of
    /// pick peaks (dB), and the shelf gain (target and smoothed).
    vel_peak: f64,
    vel_wait: usize,
    vel_avg_db: f64,
    shelf_db: f64,
    shelf_s: f64,
    shelf_a: f64,
    shelf: Biquad,
    tick: usize,
}

impl Shaper {
    fn new(fs: f64) -> Self {
        let mut split = [Biquad::default(); 3];
        for (b, f) in split.iter_mut().zip(SPLIT_HZ) {
            b.low_pass(fs, f, 0.7071);
        }
        let mut shelf = Biquad::default();
        shelf.high_shelf(fs, 2500.0, 0.0);
        Shaper {
            fs,
            split,
            env: [0.0; 4],
            slow: [0.0; 4],
            cap: [0.0; 4],
            gain: [1.0; 4],
            env_a: coef(fs, 1.0),
            env_r: [coef(fs, 40.0), coef(fs, 20.0), coef(fs, 10.0), coef(fs, 10.0)],
            slow_a: coef(fs, 30.0),
            gain_a: [coef(fs, 8.0), coef(fs, 4.0), coef(fs, 2.0), coef(fs, 1.0)],
            decay_k: DECAY_DB_S.map(|r| db(-r / fs)),
            pick_fast: 0.0,
            pick_slow: 0.0,
            pick_fa: coef(fs, 0.5),
            pick_fr: coef(fs, 50.0),
            pick_sa: coef(fs, 40.0),
            hold: 0,
            since: 0.0,
            since_k: (-1000.0 / (fs * ATTACK_MS)).exp(),
            vel_peak: 0.0,
            vel_wait: 0,
            vel_avg_db: f64::NAN,
            shelf_db: 0.0,
            shelf_s: 0.0,
            shelf_a: coef(fs, 20.0),
            shelf,
            tick: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64, decay: bool, attack: bool, dynamic: bool) -> f64 {
        // pick detector: the fast envelope jumping 5 dB over the slow one, above -50 dBFS so
        // the noise floor between phrases doesn't count
        let a = x.abs();
        let k = if a > self.pick_fast { self.pick_fa } else { self.pick_fr };
        self.pick_fast += k * (a - self.pick_fast);
        self.pick_slow += self.pick_sa * (self.pick_fast - self.pick_slow);
        let pick = self.hold == 0 && self.pick_fast > 0.003 && self.pick_fast > 1.8 * self.pick_slow;
        if pick {
            self.hold = (self.fs * 0.03) as usize;
            self.since = 1.0;
            self.vel_peak = 0.0;
            self.vel_wait = (self.fs * 0.005) as usize;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        self.since *= self.since_k;
        // DYNAMIC judges each pick 5 ms in, against the running average of recent picks
        if self.vel_wait > 0 {
            self.vel_peak = self.vel_peak.max(self.pick_fast);
            self.vel_wait -= 1;
            if self.vel_wait == 0 {
                let peak_db = 20.0 * (self.vel_peak + 1e-7).log10();
                if self.vel_avg_db.is_nan() {
                    self.vel_avg_db = peak_db;
                }
                let rel = peak_db - self.vel_avg_db;
                self.shelf_db = (DYN_DB_PER_DB * rel - DYN_OFFSET_DB).clamp(-DYN_MAX_DB, DYN_MAX_DB);
                // catch up quickly with harder playing, forget it slowly
                self.vel_avg_db += if rel > 0.0 { 0.5 } else { 0.15 } * rel;
            }
        }

        let mut y = x;
        if decay || attack {
            let mut b = [0.0f64; 4];
            let mut r = x;
            for (bi, f) in b.iter_mut().zip(self.split.iter_mut()) {
                *bi = f.process(r);
                r -= *bi;
            }
            b[3] = r;
            y = 0.0;
            for i in 0..4 {
                let a = b[i].abs();
                let k = if a > self.env[i] { self.env_a } else { self.env_r[i] };
                self.env[i] += k * (a - self.env[i]);
                self.slow[i] += self.slow_a * (self.env[i] - self.slow[i]);
                let e = self.env[i] + 1e-9;
                let mut g = 1.0;
                if decay {
                    // the cap follows the band up whenever it jumps 3 dB over its recent
                    // level (new energy from a pick in this band; a pick elsewhere doesn't
                    // refresh it), then falls at the band's acoustic decay rate
                    if e > 1.4 * self.slow[i] {
                        self.cap[i] = self.cap[i].max(e);
                    } else {
                        self.cap[i] *= self.decay_k[i];
                    }
                    if e > self.cap[i] {
                        g = (self.cap[i] / e).max(0.01);
                    }
                }
                if attack {
                    g *= db(ATTACK_DB[i] * self.since);
                }
                self.gain[i] += self.gain_a[i] * (g - self.gain[i]);
                y += self.gain[i] * b[i];
            }
        }
        if dynamic {
            self.shelf_s += self.shelf_a * (self.shelf_db - self.shelf_s);
            self.tick += 1;
            if self.tick >= 32 {
                self.tick = 0;
                self.shelf.high_shelf(self.fs, 2500.0, self.shelf_s);
            }
            y = self.shelf.process(y);
        }
        y
    }
}

/// Crossovers of the stereo pair's side bands.
const PAIR_SPLIT_HZ: [f64; 2] = [400.0, 3000.0];
/// Side level per band relative to the mid, set so the Ibanez preset's L/R coherence follows
/// the Ibanez recording's (0.95 at 100-250 Hz, 0.83, 0.59, 0.31, 0.1 at 4-10 kHz).
const PAIR_SIDE: [f64; 3] = [0.03, 0.2, 0.75];
/// Each mic's reflection pattern: length, number of reflections, and decay time constant.
const PAIR_MS: f64 = 400.0;
const PAIR_TAPS: usize = 400;
const PAIR_DECAY_MS: f64 = 150.0;

/// A spaced stereo pair in a room: both mics hear the guitar alike, but each hears its own
/// pattern of reflections. The difference between them (the side) is the mid run through a
/// sparse random-sign reflection pattern ("velvet noise"), weighted per band: long
/// wavelengths reach both mics alike, short ones don't. Left = mid + side, right = mid -
/// side, so the mono sum is the mid alone.
struct Pair {
    split: [Biquad; 2],
    /// Reflection delays (samples) and gains.
    tap_d: [usize; PAIR_TAPS],
    tap_g: [f64; PAIR_TAPS],
    /// History written twice, so a delay never wraps.
    hist: Vec<f64>,
    pos: usize,
}

impl Pair {
    fn new(fs: f64) -> Self {
        let mut split = [Biquad::default(); 2];
        for (b, f) in split.iter_mut().zip(PAIR_SPLIT_HZ) {
            b.low_pass(fs, f, 0.7071);
        }
        let len = (fs * PAIR_MS / 1000.0) as usize;
        let mut rng = Rng(0x7C3E_51A9);
        let (mut tap_d, mut tap_g) = ([0usize; PAIR_TAPS], [0.0f64; PAIR_TAPS]);
        let mut energy = 0.0;
        for k in 0..PAIR_TAPS {
            // one reflection per segment, after the first millisecond, decaying like a small room
            let seg = len as f64 / PAIR_TAPS as f64;
            let d = (fs * 0.001) as usize + ((k as f64 + rng.next()) * seg) as usize;
            let sign = if rng.next() < 0.5 { 1.0 } else { -1.0 };
            let g = sign * (-(d as f64) / (fs * PAIR_DECAY_MS / 1000.0)).exp();
            tap_d[k] = d;
            tap_g[k] = g;
            energy += g * g;
        }
        for g in tap_g.iter_mut() {
            *g /= energy.sqrt();
        }
        Pair { split, tap_d, tap_g, hist: vec![0.0; 2 * (len + (fs * 0.001) as usize + 2)], pos: 0 }
    }

    #[inline]
    fn process(&mut self, mid: f64) -> f64 {
        let lo = self.split[0].process(mid);
        let rest = mid - lo;
        let md = self.split[1].process(rest);
        let x = PAIR_SIDE[0] * lo + PAIR_SIDE[1] * md + PAIR_SIDE[2] * (rest - md);
        let n = self.hist.len() / 2;
        self.pos = (self.pos + 1) % n;
        self.hist[self.pos] = x;
        self.hist[self.pos + n] = x;
        let now = self.pos + n;
        let mut y = 0.0;
        for (&d, &g) in self.tap_d.iter().zip(self.tap_g.iter()) {
            y += g * self.hist[now - d];
        }
        y
    }
}

pub struct AcousticSim {
    fs: f64,
    fir: BodyFir,
    /// The stereo pair, and its side output for the block.
    pair: Pair,
    side: Vec<f32>,
    shaper: Shaper,
    /// Pickup correction: rumble high-pass, coil-resonance cut, top shelf, band limit.
    pick: [Biquad; 4],
    spark_in: [Biquad; 2],
    spark_out: [Biquad; 2],
    spark_env: f64,
    spark_a: f64,
    res: [Biquad; 3],
    eq: [Biquad; 4],
    last: [f32; 4],
    last_bright: bool,
    s_body: f64,
    s_level: f64,
    s_reso: f64,
    s_air: f64,
    smooth: f64,
}

impl AcousticSim {
    /// Builds the body IR and filters; call off the audio thread. `max_block` sizes the
    /// stereo pair's side output.
    pub fn new(fs: f64, max_block: usize) -> Self {
        let mut pick = [Biquad::default(); 4];
        pick[0].high_pass(fs, 70.0, 0.7);
        pick[1].peak(fs, 2540.0, 4.0, -15.0);
        pick[2].high_shelf(fs, 2920.0, 15.0);
        pick[3].low_pass(fs, 17000.0f64.min(0.45 * fs), 0.7);
        let mut spark_in = [Biquad::default(); 2];
        let mut spark_out = [Biquad::default(); 2];
        for b in spark_in.iter_mut() {
            b.high_pass(fs, 2500.0, 0.7);
        }
        for b in spark_out.iter_mut() {
            b.high_pass(fs, 5000.0, 0.7);
        }
        // air cavity (Helmholtz) and the first two top-plate modes
        let mut res = [Biquad::default(); 3];
        res[0].band_pass(fs, 98.0, 5.0);
        res[1].band_pass(fs, 196.0, 7.0);
        res[2].band_pass(fs, 232.0, 8.0);
        AcousticSim {
            fs,
            fir: BodyFir::new(&body_ir(fs), fs),
            pair: Pair::new(fs),
            side: vec![0.0; max_block.max(1)],
            shaper: Shaper::new(fs),
            pick,
            spark_in,
            spark_out,
            spark_env: 0.0,
            spark_a: 1.0 - (-1.0 / (fs * 0.01)).exp(),
            res,
            eq: [Biquad::default(); 4],
            last: [-1.0; 4],
            last_bright: false,
            s_body: -1.0,
            s_level: 0.0,
            s_reso: 0.0,
            s_air: 0.0,
            smooth: 1.0 - (-1.0 / (fs * 0.02)).exp(),
        }
    }

    fn update(&mut self, p: &AmpState) {
        let fs = self.fs;
        let k = [p.bass, p.middle, p.treble, p.presence];
        if k != self.last || p.bright != self.last_bright {
            self.last = k;
            self.last_bright = p.bright;
            self.eq[0].low_shelf(fs, 120.0, -9.0 + 18.0 * p.bass as f64);
            self.eq[1].peak(fs, 700.0, 0.8, -8.0 + 16.0 * p.middle as f64);
            self.eq[2].high_shelf(fs, 4000.0, -10.0 + 20.0 * p.treble as f64);
            self.eq[3].high_shelf(fs, 9000.0, -4.0 + 10.0 * p.presence as f64 + if p.bright { 4.0 } else { 0.0 });
        }
    }

    /// The stereo pair's side signal for the last block: left = mid + side, right = mid - side.
    pub fn side(&self, n: usize) -> &[f32] {
        &self.side[..n]
    }

    /// Processes a mono block in place (the pair's mid; the side goes to `side`).
    pub fn process(&mut self, buf: &mut [f32], p: &AmpState) {
        self.update(p);
        let t_body = p.gain as f64;
        let t_level = db(-20.0 + 30.0 * p.master as f64) * MAKEUP;
        let t_reso = 2.0 * (p.depth as f64).powi(2);
        let t_air = 0.5 * p.presence as f64;
        if self.s_body < 0.0 {
            // first block: start at the targets instead of fading in
            (self.s_body, self.s_level, self.s_reso, self.s_air) = (t_body, t_level, t_reso, t_air);
        }
        for (i, s) in buf.iter_mut().enumerate() {
            self.s_body += self.smooth * (t_body - self.s_body);
            self.s_level += self.smooth * (t_level - self.s_level);
            self.s_reso += self.smooth * (t_reso - self.s_reso);
            self.s_air += self.smooth * (t_air - self.s_air);

            let mut x = *s as f64;
            for b in self.pick.iter_mut() {
                x = b.process(x);
            }
            // sparkle: a level-independent 2nd-harmonic generator on the top octaves,
            // for the string zing a magnetic pickup doesn't hear
            let e0 = self.spark_in[0].process(x);
            let e = self.spark_in[1].process(e0);
            self.spark_env += self.spark_a * (e.abs() - self.spark_env);
            let d = e * e / (self.spark_env + 1e-5);
            let d0 = self.spark_out[0].process(d);
            x += self.s_air * self.spark_out[1].process(d0);
            if p.decay || p.attack || p.dynamic {
                x = self.shaper.process(x, p.decay, p.attack, p.dynamic);
            }

            let (head, tail) = self.fir.process(x as f32);
            let mut y = head as f64 + self.s_body * tail as f64;
            y += self.s_reso * (self.res[0].process(y) + 0.7 * self.res[1].process(y) + 0.4 * self.res[2].process(y));
            for b in self.eq.iter_mut() {
                y = b.process(y);
            }
            *s = (y * self.s_level) as f32;
            self.side[i] = (self.pair.process(y) * self.s_level) as f32;
        }
    }
}
