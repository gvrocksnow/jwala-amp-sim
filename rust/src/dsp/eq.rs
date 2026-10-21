//! Graphic EQ after the cab: nine octave-spaced constant-Q peaking bands (63 Hz–16 kHz,
//! ±12 dB) and an output level, in stereo. Slider moves are smoothed so dragging doesn't zipper,
//! and switching it on or off crossfades.

use super::filters::Biquad;
use super::state::EqState;
use crate::spec;

/// Bandwidth of one octave, so neighbouring bands meet near their -3 dB points.
const Q: f64 = 1.41;
/// Coefficients are recomputed every this many samples while a slider is moving.
const SUB: usize = 16;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Peaking bands for these slider values; `fs` limits the top band below Nyquist.
fn band(fs: f64, i: usize, gain_db: f64) -> Biquad {
    let mut b = Biquad::default();
    b.peak(fs, spec::EQ_FREQS[i].min(0.42 * fs), Q, gain_db);
    b
}

/// The EQ's response in dB at `f` Hz for the slider values `p` (bands, then LEVEL), as the UI draws it.
pub fn response_db(p: &[f32], fs: f64, f: f64) -> f64 {
    let bands: f64 = (0..spec::EQ_BANDS).map(|i| band(fs, i, spec::eq_db(p[i])).magnitude_db(fs, f)).sum();
    bands + spec::eq_db(p[spec::EQ_BANDS])
}

pub struct GraphicEq {
    fs: f64,
    l: [Biquad; spec::EQ_BANDS],
    r: [Biquad; spec::EQ_BANDS],
    /// Smoothed band gains (dB) the coefficients were last computed for.
    cur: [f64; spec::EQ_BANDS],
    level: f64,
    /// 0 = bypassed, 1 = on.
    mix: f64,
    /// Per-sub-block smoothing factor (≈30 ms) and per-sample bypass ramp step (≈20 ms).
    glide: f64,
    ramp: f64,
}

impl GraphicEq {
    pub fn new(fs: f64, st: &EqState) -> Self {
        let mut eq = GraphicEq {
            fs,
            l: Default::default(),
            r: Default::default(),
            cur: [0.0; spec::EQ_BANDS],
            level: db(spec::eq_db(st.p[spec::EQ_BANDS])),
            mix: if st.on { 1.0 } else { 0.0 },
            glide: 1.0 - (-(SUB as f64) / (fs * 0.03)).exp(),
            ramp: 1.0 / (fs * 0.02),
        };
        for i in 0..spec::EQ_BANDS {
            eq.cur[i] = spec::eq_db(st.p[i]);
        }
        eq.update_coefs();
        eq
    }

    fn update_coefs(&mut self) {
        for i in 0..spec::EQ_BANDS {
            // keep the filter state, swap only the coefficients
            let b = band(self.fs, i, self.cur[i]);
            self.l[i].copy_coefs(&b);
            self.r[i].copy_coefs(&b);
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], st: &EqState) {
        let target_mix = if st.on { 1.0 } else { 0.0 };
        if self.mix == 0.0 && target_mix == 0.0 {
            // bypassed: snap to the current settings so switching on starts from them, clean
            for i in 0..spec::EQ_BANDS {
                self.cur[i] = spec::eq_db(st.p[i]);
            }
            self.level = db(spec::eq_db(st.p[spec::EQ_BANDS]));
            for b in self.l.iter_mut().chain(self.r.iter_mut()) {
                b.reset();
            }
            self.update_coefs();
            return;
        }
        let target: [f64; spec::EQ_BANDS] = std::array::from_fn(|i| spec::eq_db(st.p[i]));
        let target_level = db(spec::eq_db(st.p[spec::EQ_BANDS]));
        let mut off = 0;
        while off < l.len() {
            let n = (l.len() - off).min(SUB);
            if self.cur.iter().zip(&target).any(|(c, t)| (c - t).abs() > 1e-3) {
                for (c, t) in self.cur.iter_mut().zip(&target) {
                    *c += self.glide * (t - *c);
                    if (*c - t).abs() <= 1e-3 {
                        *c = *t;
                    }
                }
                self.update_coefs();
            }
            for k in off..off + n {
                self.level += self.glide / SUB as f64 * (target_level - self.level);
                self.mix = if self.mix < target_mix { (self.mix + self.ramp).min(1.0) } else { (self.mix - self.ramp).max(0.0) };
                let (dl, dr) = (l[k] as f64, r[k] as f64);
                let (mut wl, mut wr) = (dl, dr);
                for (bl, br) in self.l.iter_mut().zip(self.r.iter_mut()) {
                    wl = bl.process(wl);
                    wr = br.process(wr);
                }
                l[k] = (dl + self.mix * (wl * self.level - dl)) as f32;
                r[k] = (dr + self.mix * (wr * self.level - dr)) as f32;
            }
            off += n;
        }
    }
}
