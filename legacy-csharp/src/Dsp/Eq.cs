using System;
using System.Linq;

namespace Jwala.Dsp;

/// <summary>Graphic EQ settings: nine octave bands (63 Hz–16 kHz, ±12 dB), then LEVEL. Same keys as the Rust version.</summary>
public sealed class EqParams
{
    public const int Bands = 9;
    /// <summary>Centre frequencies, one octave apart.</summary>
    public static readonly double[] Freqs = { 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000 };
    /// <summary>Slider travel in each direction.</summary>
    public const double RangeDb = 12;

    public volatile bool Enabled;
    /// <summary>The bands, then LEVEL.</summary>
    public readonly Param[] Knobs = new[] { "63", "125", "250", "500", "1K", "2K", "4K", "8K", "16K", "LEVEL" }
        .Select(n => new Param(n, 0.5f, FmtDb)).ToArray();

    /// <summary>Slider value (0..1, centre = flat) to dB.</summary>
    public static double ToDb(float v) => (v * 2.0 - 1.0) * RangeDb;
    static string FmtDb(float v) { double d = ToDb(v); return Math.Abs(d) < 0.05 ? "0.0 dB" : d.ToString("+0.0;-0.0") + " dB"; }
}

/// <summary>
/// Graphic EQ after the cab: nine octave-spaced bands (63 Hz–16 kHz, ±12 dB) and an output
/// level, in stereo.
///
/// Each band is an analog-matched bell (Biquad.PeakMatched), so the top bands keep their
/// analog shape up to Nyquist. The bells are broad and overlap, which sounds smooth and musical,
/// but overlapping bands add up; so the gains the filters actually run at are solved from the
/// sliders (Välimäki &amp; Liski, "Accurate Cascade Graphic Equalizer", IEEE SPL 2017): a
/// least-squares fit at the band centres and the midpoints between them, refined once with the
/// band shapes at the first fit's gains. The curve then follows the sliders to within a fraction
/// of a dB, however neighbours are set. Slider moves are smoothed so dragging doesn't zipper, and
/// switching it on or off crossfades.
/// </summary>
public sealed class GraphicEq
{
    const int N = EqParams.Bands;
    /// <summary>The band centres and the midpoints between neighbours, where the response is fitted.</summary>
    const int Pts = 2 * N - 1;
    /// <summary>Band Q. Broader than an octave, so neighbours blend into one smooth curve.</summary>
    const double Q = 0.9;
    /// <summary>Gain the first fit measures the band shapes at.</summary>
    const double Gp = 17.0;
    /// <summary>Solved gains stay within this, so even a jagged zig-zag of sliders keeps sane filters.</summary>
    const double MaxDb = 24.0;
    /// <summary>Coefficients are recomputed every this many samples while a slider is moving.</summary>
    const int Sub = 16;

    readonly double fs;
    readonly EqParams p;
    readonly Biquad[] l = new Biquad[EqParams.Bands], r = new Biquad[EqParams.Bands];
    /// <summary>Smoothed band gains (dB) the coefficients were last computed for.</summary>
    readonly double[] cur = new double[EqParams.Bands], target = new double[EqParams.Bands];
    double level;
    /// <summary>0 = bypassed, 1 = on.</summary>
    double mix;
    /// <summary>Per-sub-block smoothing factor (≈30 ms) and per-sample bypass ramp step (≈20 ms).</summary>
    readonly double glide, ramp;

    static double Db(double db) => Math.Pow(10, db / 20);

    /// <summary>Band <paramref name="i"/>'s centre; <paramref name="fs"/> keeps the top band below Nyquist.</summary>
    static double Centre(double fs, int i) => Math.Min(EqParams.Freqs[i], 0.42 * fs);

    static Biquad Band(double fs, int i, double gainDb)
    {
        var b = new Biquad();
        b.PeakMatched(fs, Centre(fs, i), Q, gainDb);
        return b;
    }

    /// <summary>Least squares: the g minimising |B g - t|, via the normal equations.</summary>
    static double[] Fit(double[,] b, double[] t)
    {
        var m = new double[N, N + 1];
        for (int i = 0; i < N; i++)
        {
            for (int j = 0; j < N; j++) { double s = 0; for (int k = 0; k < Pts; k++) s += b[k, i] * b[k, j]; m[i, j] = s; }
            double st = 0; for (int k = 0; k < Pts; k++) st += b[k, i] * t[k]; m[i, N] = st;
        }
        for (int c = 0; c < N; c++)
        {
            int p = c;
            for (int x = c + 1; x < N; x++) if (Math.Abs(m[x, c]) >= Math.Abs(m[p, c])) p = x;
            if (p != c) for (int k = 0; k <= N; k++) (m[c, k], m[p, k]) = (m[p, k], m[c, k]);
            double d = m[c, c];
            if (Math.Abs(d) < 1e-12) continue;
            for (int r = 0; r < N; r++)
            {
                if (r == c) continue;
                double f = m[r, c] / d;
                for (int k = c; k <= N; k++) m[r, k] -= f * m[c, k];
            }
        }
        var g = new double[N];
        for (int i = 0; i < N; i++) g[i] = Math.Abs(m[i, i]) < 1e-12 ? 0 : m[i, N] / m[i, i];
        return g;
    }

    /// <summary>The gains (dB) the bands run at so the summed curve follows the band sliders.</summary>
    static double[] Solve(double fs, float[] p, int passes = 2)
    {
        var s = new double[N];
        bool flat = true;
        for (int i = 0; i < N; i++) { s[i] = EqParams.ToDb(p[i]); if (Math.Abs(s[i]) >= 1e-9) flat = false; }
        if (flat) return new double[N];
        var f = new double[Pts]; var t = new double[Pts];
        for (int k = 0; k < Pts; k++)
        {
            f[k] = k % 2 == 0 ? Centre(fs, k / 2) : Math.Sqrt(Centre(fs, k / 2) * Centre(fs, k / 2 + 1));
            t[k] = k % 2 == 0 ? s[k / 2] : 0.5 * (s[k / 2] + s[k / 2 + 1]);
        }
        // each band's response per dB of gain, measured at gain g[i]. A symmetric bell's cut is the
        // mirror of its boost, so only the size of the gain matters.
        double[,] Shapes(double[] g)
        {
            var b = new double[Pts, N];
            for (int i = 0; i < N; i++)
            {
                double gi = Math.Max(Math.Abs(g[i]), 0.5);
                var bq = Band(fs, i, gi);
                for (int k = 0; k < Pts; k++) b[k, i] = bq.MagnitudeDb(fs, f[k]) / gi;
            }
            return b;
        }
        var g0 = new double[N]; Array.Fill(g0, Gp);
        var gs = Fit(Shapes(g0), t);
        for (int pass = 1; pass < passes; pass++) gs = Fit(Shapes(gs), t);
        for (int i = 0; i < N; i++) gs[i] = Math.Clamp(gs[i], -MaxDb, MaxDb);
        return gs;
    }

    /// <summary>The EQ's response for the slider values (bands, then LEVEL), as the UI draws it: a
    /// function from Hz to dB.</summary>
    public static Func<double, double> Response(float[] v, double fs)
    {
        var g = Solve(fs, v);
        var bands = new Biquad[N];
        for (int i = 0; i < N; i++) bands[i] = Band(fs, i, g[i]);
        double level = EqParams.ToDb(v[N]);
        return f => { double sum = level; foreach (var b in bands) sum += b.MagnitudeDb(fs, f); return sum; };
    }

    /// <summary>The band sliders the target was solved for.</summary>
    readonly float[] sliders = new float[N];

    /// <summary>Re-solves the band gains when a slider moved.</summary>
    void Retarget()
    {
        bool changed = false;
        for (int i = 0; i < N; i++) if (p.Knobs[i].Value != sliders[i]) { sliders[i] = p.Knobs[i].Value; changed = true; }
        if (changed) Array.Copy(Solve(fs, sliders), target, N);
    }

    public GraphicEq(double fs, EqParams p)
    {
        this.fs = fs; this.p = p;
        level = Db(EqParams.ToDb(p.Knobs[EqParams.Bands].Value));
        mix = p.Enabled ? 1 : 0;
        glide = 1 - Math.Exp(-(double)Sub / (fs * 0.03));
        ramp = 1 / (fs * 0.02);
        for (int i = 0; i < N; i++) sliders[i] = p.Knobs[i].Value;
        Array.Copy(Solve(fs, sliders), target, N);
        Array.Copy(target, cur, N);
        UpdateCoefs();
    }

    void UpdateCoefs()
    {
        for (int i = 0; i < EqParams.Bands; i++)
        {
            // keep the filter state, swap only the coefficients
            var b = Band(fs, i, cur[i]);
            l[i].CopyCoefs(b); r[i].CopyCoefs(b);
        }
    }

    public void Process(Span<float> lb, Span<float> rb)
    {
        Retarget();
        double targetMix = p.Enabled ? 1 : 0;
        if (mix == 0 && targetMix == 0)
        {
            // bypassed: snap to the current settings so switching on starts from them, clean
            for (int i = 0; i < N; i++) { cur[i] = target[i]; l[i].Reset(); r[i].Reset(); }
            level = Db(EqParams.ToDb(p.Knobs[EqParams.Bands].Value));
            UpdateCoefs();
            return;
        }
        double targetLevel = Db(EqParams.ToDb(p.Knobs[EqParams.Bands].Value));
        for (int off = 0; off < lb.Length; off += Sub)
        {
            int n = Math.Min(lb.Length - off, Sub);
            bool moving = false;
            for (int i = 0; i < EqParams.Bands; i++) if (Math.Abs(cur[i] - target[i]) > 1e-3) moving = true;
            if (moving)
            {
                for (int i = 0; i < EqParams.Bands; i++)
                {
                    cur[i] += glide * (target[i] - cur[i]);
                    if (Math.Abs(cur[i] - target[i]) <= 1e-3) cur[i] = target[i];
                }
                UpdateCoefs();
            }
            for (int k = off; k < off + n; k++)
            {
                level += glide / Sub * (targetLevel - level);
                mix = mix < targetMix ? Math.Min(mix + ramp, 1) : Math.Max(mix - ramp, 0);
                double dl = lb[k], dr = rb[k], wl = dl, wr = dr;
                for (int i = 0; i < EqParams.Bands; i++) { wl = l[i].Process(wl); wr = r[i].Process(wr); }
                lb[k] = (float)(dl + mix * (wl * level - dl));
                rb[k] = (float)(dr + mix * (wr * level - dr));
            }
        }
    }
}
