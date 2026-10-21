//! Chromatic tuner. The audio thread feeds raw input into a lock-free ring buffer; the UI thread
//! periodically runs YIN pitch detection on the most recent window.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

const RING_SIZE: usize = 16384;
const DECIM: usize = 2;
const WINDOW: usize = 1024;
const MIN_HZ: f64 = 50.0;
const MAX_HZ: f64 = 1200.0;

/// Shared between the audio thread (writer) and the UI (reader).
pub struct TunerShared {
    ring: Vec<AtomicU32>,
    write_pos: AtomicUsize,
    pub active: AtomicBool,
    pub mute_output: AtomicBool,
}

impl Default for TunerShared {
    fn default() -> Self {
        TunerShared {
            ring: (0..RING_SIZE).map(|_| AtomicU32::new(0)).collect(),
            write_pos: AtomicUsize::new(0),
            active: AtomicBool::new(false),
            mute_output: AtomicBool::new(true),
        }
    }
}

impl TunerShared {
    /// Audio thread: append input samples (only while the tuner is open).
    pub fn write(&self, x: &[f32]) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let mut w = self.write_pos.load(Ordering::Relaxed);
        for &v in x {
            self.ring[w].store(v.to_bits(), Ordering::Relaxed);
            w = (w + 1) & (RING_SIZE - 1);
        }
        self.write_pos.store(w, Ordering::Release);
    }

    fn at(&self, i: usize) -> f32 {
        f32::from_bits(self.ring[i & (RING_SIZE - 1)].load(Ordering::Relaxed))
    }
}

/// UI-side analysis state.
pub struct Tuner {
    pub reference_a: f64,
    pub frequency: f64,
    pub has_signal: bool,
    recent: [f64; 5],
    recent_count: usize,
    recent_idx: usize,
    smoothed: f64,
    buf: Vec<f32>,
    d: Vec<f64>,
}

impl Default for Tuner {
    fn default() -> Self {
        Tuner {
            reference_a: 440.0,
            frequency: 0.0,
            has_signal: false,
            recent: [0.0; 5],
            recent_count: 0,
            recent_idx: 0,
            smoothed: 0.0,
            buf: vec![0.0; WINDOW * 2 + 8],
            d: vec![0.0; WINDOW],
        }
    }
}

const NAMES: [&str; 12] = ["C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B"];

impl Tuner {
    /// Analyse the latest audio. Returns true when a stable pitch was found.
    pub fn analyze(&mut self, shared: &TunerShared, sample_rate: f64) -> bool {
        // pull the newest 2*WINDOW decimated samples (simple 2-tap average as anti-alias)
        let need = self.buf.len() * DECIM;
        let wp = shared.write_pos.load(Ordering::Acquire);
        let start = (wp + RING_SIZE - need) & (RING_SIZE - 1);
        let mut sum2 = 0.0;
        for i in 0..self.buf.len() {
            let k = start + i * DECIM;
            let v = 0.5 * (shared.at(k) + shared.at(k + 1));
            self.buf[i] = v;
            sum2 += (v * v) as f64;
        }
        let rms = (sum2 / self.buf.len() as f64).sqrt();
        if rms < 0.002 {
            self.lose(); // ~ -54 dBFS: no note
            return false;
        }

        let fs = sample_rate / DECIM as f64;
        let tau_min = (fs / MAX_HZ) as usize;
        let tau_max = (WINDOW - 1).min((fs / MIN_HZ) as usize);

        // YIN: difference function + cumulative mean normalisation
        self.d[0] = 1.0;
        let mut running = 0.0;
        for t in 1..=tau_max {
            let mut s = 0.0;
            for j in 0..WINDOW {
                let diff = (self.buf[j] - self.buf[j + t]) as f64;
                s += diff * diff;
            }
            running += s;
            self.d[t] = if running > 0.0 { s * t as f64 / running } else { 1.0 };
        }
        let mut tau = None;
        let mut t = tau_min;
        while t <= tau_max {
            if self.d[t] < 0.12 {
                while t < tau_max && self.d[t + 1] < self.d[t] {
                    t += 1;
                }
                tau = Some(t);
                break;
            }
            t += 1;
        }
        let Some(tau) = tau else {
            self.lose();
            return false;
        };

        // parabolic interpolation around the minimum
        let mut better = tau as f64;
        if tau > 1 && tau < tau_max {
            let (a, b, c) = (self.d[tau - 1], self.d[tau], self.d[tau + 1]);
            let den = a + c - 2.0 * b;
            if den.abs() > 1e-12 {
                better = tau as f64 + 0.5 * (a - c) / den;
            }
        }
        let f = fs / better;

        // median of the last few readings rejects octave blips; then light smoothing
        self.recent[self.recent_idx] = f;
        self.recent_idx = (self.recent_idx + 1) % self.recent.len();
        self.recent_count = (self.recent_count + 1).min(self.recent.len());
        let mut sorted = self.recent;
        let sorted = &mut sorted[..self.recent_count];
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = sorted[self.recent_count / 2];
        if !self.has_signal || (1200.0 * (med / self.smoothed).log2()).abs() > 50.0 {
            self.smoothed = med;
        } else {
            self.smoothed += 0.35 * (med - self.smoothed);
        }
        self.frequency = self.smoothed;
        self.has_signal = true;
        true
    }

    fn lose(&mut self) {
        self.has_signal = false;
        self.recent_count = 0;
    }

    /// Nearest note name, octave and deviation in cents for a frequency.
    pub fn describe(&self, f: f64) -> (&'static str, i32, f64) {
        let midi = 69.0 + 12.0 * (f / self.reference_a).log2();
        let n = midi.round() as i32;
        (NAMES[n.rem_euclid(12) as usize], n / 12 - 1, (midi - n as f64) * 100.0)
    }
}
