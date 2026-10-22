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
/// Graphic EQ after the cab: nine octave-spaced constant-Q peaking bands and an output level, in stereo. Slider moves
/// are smoothed so dragging doesn't zipper, and switching it on or off crossfades.
/// </summary>
public sealed class GraphicEq
{
    /// <summary>Bandwidth of one octave, so neighbouring bands meet near their -3 dB points.</summary>
    const double Q = 1.41;
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

    /// <summary>Peaking band for a slider value; <paramref name="fs"/> limits the top band below Nyquist.</summary>
    static Biquad Band(double fs, int i, double gainDb)
    {
        var b = new Biquad();
        b.Peak(fs, Math.Min(EqParams.Freqs[i], 0.42 * fs), Q, gainDb);
        return b;
    }

    /// <summary>The EQ's response in dB at <paramref name="f"/> Hz for slider values (bands, then LEVEL), as the UI draws it.</summary>
    public static double ResponseDb(float[] v, double fs, double f)
    {
        double sum = EqParams.ToDb(v[EqParams.Bands]);
        for (int i = 0; i < EqParams.Bands; i++) sum += Band(fs, i, EqParams.ToDb(v[i])).MagnitudeDb(fs, f);
        return sum;
    }

    public GraphicEq(double fs, EqParams p)
    {
        this.fs = fs; this.p = p;
        level = Db(EqParams.ToDb(p.Knobs[EqParams.Bands].Value));
        mix = p.Enabled ? 1 : 0;
        glide = 1 - Math.Exp(-(double)Sub / (fs * 0.03));
        ramp = 1 / (fs * 0.02);
        for (int i = 0; i < EqParams.Bands; i++) cur[i] = EqParams.ToDb(p.Knobs[i].Value);
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
        double targetMix = p.Enabled ? 1 : 0;
        if (mix == 0 && targetMix == 0)
        {
            // bypassed: snap to the current settings so switching on starts from them, clean
            for (int i = 0; i < EqParams.Bands; i++) { cur[i] = EqParams.ToDb(p.Knobs[i].Value); l[i].Reset(); r[i].Reset(); }
            level = Db(EqParams.ToDb(p.Knobs[EqParams.Bands].Value));
            UpdateCoefs();
            return;
        }
        for (int i = 0; i < EqParams.Bands; i++) target[i] = EqParams.ToDb(p.Knobs[i].Value);
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
