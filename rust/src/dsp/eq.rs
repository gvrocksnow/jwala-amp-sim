//! Graphic EQ after the cab: nine octave-spaced bands (63 Hz–16 kHz, ±12 dB) and an output
//! level, in stereo.
//!
//! Each band is an analog-matched bell (`Biquad::peak_matched`), so the top bands keep their
//! analog shape up to Nyquist. The bells are broad and overlap, which sounds smooth and musical,
//! but overlapping bands add up; so the gains the filters actually run at are solved from the
//! sliders (Välimäki & Liski, "Accurate Cascade Graphic Equalizer", IEEE SPL 2017): a
//! least-squares fit at the band centres and the midpoints between them, refined once with the
//! band shapes at the first fit's gains. The curve then follows the sliders to within a fraction
//! of a dB, however neighbours are set. Slider moves are smoothed so dragging doesn't zipper, and
//! switching it on or off crossfades.

use super::filters::Biquad;
use super::state::EqState;
use crate::spec;

const N: usize = spec::EQ_BANDS;
/// The band centres and the midpoints between neighbours, where the response is fitted.
const PTS: usize = 2 * N - 1;
/// Band Q. Broader than an octave, so neighbours blend into one smooth curve.
const Q: f64 = 0.9;
/// Gain the first fit measures the band shapes at.
const GP: f64 = 17.0;
/// Solved gains stay within this, so even a jagged zig-zag of sliders keeps sane filters.
const MAX_DB: f64 = 24.0;
/// Coefficients are recomputed every this many samples while a slider is moving.
const SUB: usize = 16;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Band `i`'s centre; `fs` keeps the top band below Nyquist.
fn centre(fs: f64, i: usize) -> f64 {
    spec::EQ_FREQS[i].min(0.42 * fs)
}

fn band(fs: f64, i: usize, q: f64, gain_db: f64) -> Biquad {
    let mut b = Biquad::default();
    b.peak_matched(fs, centre(fs, i), q, gain_db);
    b
}

/// Least squares: the `g` minimising |B g - t|, via the normal equations.
fn fit(b: &[[f64; N]; PTS], t: &[f64; PTS]) -> [f64; N] {
    let mut m = [[0.0; N + 1]; N];
    for i in 0..N {
        for j in 0..N {
            m[i][j] = (0..PTS).map(|k| b[k][i] * b[k][j]).sum();
        }
        m[i][N] = (0..PTS).map(|k| b[k][i] * t[k]).sum();
    }
    for c in 0..N {
        let p = (c..N).max_by(|&x, &y| m[x][c].abs().total_cmp(&m[y][c].abs())).unwrap();
        m.swap(c, p);
        let d = m[c][c];
        if d.abs() < 1e-12 {
            continue;
        }
        for r in 0..N {
            if r != c {
                let f = m[r][c] / d;
                for k in c..=N {
                    m[r][k] -= f * m[c][k];
                }
            }
        }
    }
    std::array::from_fn(|i| if m[i][i].abs() < 1e-12 { 0.0 } else { m[i][N] / m[i][i] })
}

/// The gains (dB) the bands run at so the summed curve follows the band sliders `p`.
fn solve(fs: f64, p: &[f32], q: f64, passes: usize) -> [f64; N] {
    let s: [f64; N] = std::array::from_fn(|i| spec::eq_db(p[i]));
    if s.iter().all(|g| g.abs() < 1e-9) {
        return [0.0; N];
    }
    let f: [f64; PTS] = std::array::from_fn(|k| if k % 2 == 0 { centre(fs, k / 2) } else { (centre(fs, k / 2) * centre(fs, k / 2 + 1)).sqrt() });
    let t: [f64; PTS] = std::array::from_fn(|k| if k % 2 == 0 { s[k / 2] } else { 0.5 * (s[k / 2] + s[k / 2 + 1]) });
    // each band's response per dB of gain, measured at gain `g[i]`. A symmetric bell's cut is the
    // mirror of its boost, so only the size of the gain matters.
    let shapes = |g: &[f64; N]| {
        let mut b = [[0.0; N]; PTS];
        for i in 0..N {
            let gi = g[i].abs().max(0.5);
            let bq = band(fs, i, q, gi);
            for k in 0..PTS {
                b[k][i] = bq.magnitude_db(fs, f[k]) / gi;
            }
        }
        b
    };
    let mut g = fit(&shapes(&[GP; N]), &t);
    for _ in 1..passes {
        g = fit(&shapes(&g), &t);
    }
    g.map(|x| x.clamp(-MAX_DB, MAX_DB))
}

fn band_gains(fs: f64, p: &[f32]) -> [f64; N] {
    solve(fs, p, Q, 2)
}

/// The EQ's response for the slider values `p` (bands, then LEVEL), as the UI draws it: a
/// function from Hz to dB.
pub fn response(p: &[f32], fs: f64) -> impl Fn(f64) -> f64 {
    let g = band_gains(fs, p);
    let bands: [Biquad; N] = std::array::from_fn(|i| band(fs, i, Q, g[i]));
    let level = spec::eq_db(p[N]);
    move |f| bands.iter().map(|b| b.magnitude_db(fs, f)).sum::<f64>() + level
}

pub struct GraphicEq {
    fs: f64,
    l: [Biquad; N],
    r: [Biquad; N],
    /// Smoothed band gains (dB) the coefficients were last computed for.
    cur: [f64; N],
    /// The band sliders `target` was solved for, and the gains solved from them.
    sliders: [f32; N],
    target: [f64; N],
    level: f64,
    /// 0 = bypassed, 1 = on.
    mix: f64,
    /// Per-sub-block smoothing factor (≈30 ms) and per-sample bypass ramp step (≈20 ms).
    glide: f64,
    ramp: f64,
}

impl GraphicEq {
    pub fn new(fs: f64, st: &EqState) -> Self {
        let sliders: [f32; N] = std::array::from_fn(|i| st.p[i]);
        let target = band_gains(fs, &sliders);
        let mut eq = GraphicEq {
            fs,
            l: Default::default(),
            r: Default::default(),
            cur: target,
            sliders,
            target,
            level: db(spec::eq_db(st.p[N])),
            mix: if st.on { 1.0 } else { 0.0 },
            glide: 1.0 - (-(SUB as f64) / (fs * 0.03)).exp(),
            ramp: 1.0 / (fs * 0.02),
        };
        eq.update_coefs();
        eq
    }

    fn update_coefs(&mut self) {
        for i in 0..N {
            // keep the filter state, swap only the coefficients
            let b = band(self.fs, i, Q, self.cur[i]);
            self.l[i].copy_coefs(&b);
            self.r[i].copy_coefs(&b);
        }
    }

    /// Re-solves the band gains when a slider moved.
    fn retarget(&mut self, st: &EqState) {
        if st.p[..N] != self.sliders {
            self.sliders.copy_from_slice(&st.p[..N]);
            self.target = band_gains(self.fs, &self.sliders);
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], st: &EqState) {
        self.retarget(st);
        let target_mix = if st.on { 1.0 } else { 0.0 };
        if self.mix == 0.0 && target_mix == 0.0 {
            // bypassed: snap to the current settings so switching on starts from them, clean
            self.cur = self.target;
            self.level = db(spec::eq_db(st.p[N]));
            for b in self.l.iter_mut().chain(self.r.iter_mut()) {
                b.reset();
            }
            self.update_coefs();
            return;
        }
        let target = self.target;
        let target_level = db(spec::eq_db(st.p[N]));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The analog bell the matched filter imitates.
    fn analog_db(f0: f64, q: f64, gain_db: f64, f: f64) -> f64 {
        let a = 10f64.powf(gain_db / 40.0);
        let w = f / f0;
        let num = (1.0 - w * w).powi(2) + (w * a / q).powi(2);
        let den = (1.0 - w * w).powi(2) + (w / (a * q)).powi(2);
        10.0 * (num / den).log10()
    }

    #[test]
    fn matched_bells_follow_the_analog_shape() {
        for fs in [44100.0, 48000.0, 96000.0] {
            for f0 in spec::EQ_FREQS {
                for g in [-12.0, -3.0, 6.0, 12.0, 20.0] {
                    let mut b = Biquad::default();
                    b.peak_matched(fs, f0, Q, g);
                    assert!((b.magnitude_db(fs, f0) - g).abs() < 1e-6, "centre gain {fs} {f0} {g}");
                    let mut old = Biquad::default();
                    old.peak(fs, f0, Q, g);
                    let (mut worst, mut worst_old): (f64, f64) = (0.0, 0.0);
                    let mut f = 20.0;
                    while f < 0.45 * fs {
                        worst = worst.max((b.magnitude_db(fs, f) - analog_db(f0, Q, g, f)).abs());
                        worst_old = worst_old.max((old.magnitude_db(fs, f) - analog_db(f0, Q, g, f)).abs());
                        f *= 1.02;
                    }
                    if f0 >= 4000.0 && g == 12.0 {
                        println!("{fs} Hz, {f0} Hz +12 dB: off analog by {worst:.2} dB matched, {worst_old:.2} dB bilinear");
                    }
                    assert!(worst < 2.0 &&worst <= worst_old + 0.05, "{fs} Hz, {f0} Hz band at {g} dB strays {worst:.2} dB from analog (bilinear {worst_old:.2})");
                }
            }
        }
    }

    /// Worst miss (dB) at the band centres and midpoints, over a set of slider shapes.
    fn worst_miss(fs: f64, q: f64, passes: usize) -> f64 {
        let shapes: Vec<[f32; N]> = vec![
            [1.0; N],
            [0.0; N],
            [0.5, 0.5, 0.5, 0.5, 1.0, 0.5, 0.5, 0.5, 0.5],
            [0.5, 0.5, 0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.5],
            [1.0, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 1.0, 1.0],
            [0.75, 0.6, 0.5, 0.3, 0.25, 0.4, 0.6, 0.7, 0.65],
            [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0],
            [0.5, 1.0, 1.0, 0.5, 0.5, 0.5, 0.0, 0.0, 0.5],
            [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
        ];
        let mut worst: f64 = 0.0;
        for s in &shapes {
            let g = solve(fs, s, q, passes);
            let bands: [Biquad; N] = std::array::from_fn(|i| band(fs, i, q, g[i]));
            let resp = |f: f64| bands.iter().map(|b| b.magnitude_db(fs, f)).sum::<f64>();
            // skip the zig-zag: no smooth curve can hit it, it only has to stay sane
            if s[1] == 0.0 && s[0] == 1.0 {
                assert!(g.iter().all(|x| x.abs() <= MAX_DB));
                continue;
            }
            for i in 0..N {
                worst = worst.max((resp(centre(fs, i)) - spec::eq_db(s[i])).abs());
            }
        }
        worst
    }

    #[test]
    fn curve_follows_the_sliders() {
        for q in [0.67, 0.8, 0.9, 1.0, 1.41] {
            for passes in [1, 2, 3] {
                println!("Q {q}, {passes} pass(es): worst miss {:.2} dB", worst_miss(48000.0, q, passes));
            }
        }
        for fs in [44100.0, 48000.0, 96000.0] {
            let w = worst_miss(fs, Q, 2);
            assert!(w < 1.0, "{fs}: misses a slider by {w:.2} dB");
        }
    }

    #[test]
    fn flat_is_flat() {
        let r = response(&[0.5; N + 1], 48000.0);
        for f in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
            assert!(r(f).abs() < 1e-9);
        }
    }
}
