using System;
using System.Collections.Generic;

namespace Jwala.Dsp;

/// <summary>In-place radix-2 complex FFT.</summary>
public sealed class Fft
{
    readonly int n;
    readonly double[] cos, sin;
    readonly int[] rev;

    public Fft(int n)
    {
        if (n < 2 || (n & (n - 1)) != 0) throw new ArgumentException("FFT size must be a power of two", nameof(n));
        this.n = n;
        int bits = System.Numerics.BitOperations.TrailingZeroCount(n);
        rev = new int[n];
        for (int i = 0; i < n; i++)
        {
            int r = 0;
            for (int b = 0; b < bits; b++) if ((i & (1 << b)) != 0) r |= 1 << (bits - 1 - b);
            rev[i] = r;
        }
        cos = new double[n / 2]; sin = new double[n / 2];
        for (int k = 0; k < n / 2; k++) { cos[k] = Math.Cos(2 * Math.PI * k / n); sin[k] = Math.Sin(2 * Math.PI * k / n); }
    }

    /// <summary>Forward transform uses e^-iwt; the inverse is unscaled (divide by n yourself).</summary>
    public void Run(double[] re, double[] im, bool inverse)
    {
        for (int i = 0; i < n; i++)
        {
            int j = rev[i];
            if (j > i) { (re[i], re[j]) = (re[j], re[i]); (im[i], im[j]) = (im[j], im[i]); }
        }
        double sign = inverse ? 1 : -1;
        for (int len = 2; len <= n; len <<= 1)
        {
            int half = len / 2, step = n / len;
            for (int start = 0; start < n; start += len)
                for (int k = 0; k < half; k++)
                {
                    double c = cos[k * step], s = sign * sin[k * step];
                    int a = start + k, b = start + k + half;
                    double tr = re[b] * c - im[b] * s, ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr; im[b] = im[a] - ti;
                    re[a] += tr; im[a] += ti;
                }
        }
    }
}

/// <summary>
/// A set of IRs ready to swap into the convolver: everything is allocated off the audio thread.
///
/// The convolver splits each IR into blocks of B taps. The first block runs directly, sample by
/// sample, so there is no latency; the rest runs in the frequency domain once per block, sharing one
/// FFT of the (mono) input across every IR. Each IR has a gain into L and R, smoothed per block, which
/// is how the built-in cabs crossfade between mic positions.
/// </summary>
public sealed class PreparedIr
{
    /// <summary>Taps run directly per sample; also the FFT partition size (FFT size is 2·B).</summary>
    internal const int B = 64;
    const int Bins = B + 1;

    readonly int nIr;
    /// <summary>FFT partitions after the direct head (0 for IRs no longer than B).</summary>
    readonly int parts;
    /// <summary>First B taps of each IR.</summary>
    readonly float[] head;
    /// <summary>Spectra of each IR's later partitions: [ir][part][bin].</summary>
    readonly float[] hRe, hIm;
    /// <summary>Gain of each IR into L and R: what the mic position asks for, and the smoothed value in use.</summary>
    readonly float[] targetL, targetR, gainL, gainR;
    /// <summary>True for the built-in cabs (mic position crossfades IRs); false for one IR on both sides.</summary>
    public readonly bool Positional;
    float lastPos;
    readonly float smooth;
    bool snap = true;
    // running state
    readonly Fft fft = new(2 * B);
    readonly float[] hist = new float[2 * B];
    int hp;
    readonly float[] kernL = new float[B], kernR = new float[B];
    readonly float[] xRe, xIm;
    int xPos;
    readonly double[] frame = new double[2 * B];
    readonly float[] tailL = new float[B], tailR = new float[B];
    int pos;
    readonly double[] fRe = new double[2 * B], fIm = new double[2 * B];
    readonly double[] accLRe = new double[Bins], accLIm = new double[Bins], accRRe = new double[Bins], accRIm = new double[Bins];
    readonly double[] tRe = new double[Bins], tIm = new double[Bins];

    /// <summary>One IR, the same on both sides.</summary>
    public static PreparedIr Single(double fs, float[] ir) => new(fs, new List<float[]> { ir }, new[] { 1f }, new[] { 1f }, false);

    /// <summary>A built-in cab: Cab.Positions × (L, R) dynamic-mic IRs, then the ribbon mic's (L, R).</summary>
    internal static PreparedIr PositionalSet(double fs, List<float[]> irs)
    {
        var p = new PreparedIr(fs, irs, new float[irs.Count], new float[irs.Count], true) { lastPos = -1 };
        p.SetMic(0.3f);
        return p;
    }

    PreparedIr(double fs, List<float[]> irs, float[] gl, float[] gr, bool positional)
    {
        nIr = irs.Count;
        int len = 1;
        foreach (var v in irs) len = Math.Max(len, v.Length);
        len = Math.Clamp(len, 1, Convolver.MaxLength);
        parts = (len + B - 1) / B - 1;
        head = new float[nIr * B];
        hRe = new float[nIr * parts * Bins]; hIm = new float[nIr * parts * Bins];
        var re = new double[2 * B]; var im = new double[2 * B];
        int fade = Math.Clamp(len / 8, 1, 256);
        for (int i = 0; i < nIr; i++)
        {
            var ir = irs[i];
            // fade the truncated end so it can't click
            double Tap(int k)
            {
                double v = k < ir.Length ? ir[k] : 0;
                if (k >= len) return 0;
                if (k + fade > len) return v * 0.5 * (1 - Math.Cos(Math.PI * (len - k) / fade));
                return v;
            }
            for (int k = 0; k < B; k++) head[i * B + k] = (float)Tap(k);
            for (int p = 0; p < parts; p++)
            {
                for (int k = 0; k < 2 * B; k++) { re[k] = k < B ? Tap((p + 1) * B + k) : 0; im[k] = 0; }
                fft.Run(re, im, false);
                int o = (i * parts + p) * Bins;
                for (int k = 0; k < Bins; k++) { hRe[o + k] = (float)re[k]; hIm[o + k] = (float)im[k]; }
            }
        }
        targetL = (float[])gl.Clone(); targetR = (float[])gr.Clone();
        gainL = gl; gainR = gr;
        Positional = positional;
        smooth = (float)(1 - Math.Exp(-B / (Math.Max(fs, 1) * 0.03)));
        xRe = new float[Math.Max(parts, 1) * Bins]; xIm = new float[Math.Max(parts, 1) * Bins];
        UpdateKernels();
    }

    /// <summary>Points the built-in cab's mic at <paramref name="p"/> (0 = cap .. 1 = cone). No effect on a single IR.</summary>
    public void SetMic(float p)
    {
        if (!Positional || p == lastPos) return;
        lastPos = p;
        const int Positions = Cab.Positions;
        float t = Math.Clamp(p, 0f, 1f) * (Positions - 1);
        int k0 = Math.Min((int)t, Positions - 2);
        float f = t - k0;
        float dynamic = 1 - Cab.RibbonMix;
        for (int k = 0; k < Positions; k++)
        {
            float w = dynamic * (k == k0 ? 1 - f : k == k0 + 1 ? f : 0);
            targetL[2 * k] = w; targetR[2 * k] = 0;
            targetL[2 * k + 1] = 0; targetR[2 * k + 1] = w;
        }
        targetL[2 * Positions] = Cab.RibbonMix; targetR[2 * Positions] = 0;
        targetL[2 * Positions + 1] = 0; targetR[2 * Positions + 1] = Cab.RibbonMix;
        if (snap)
        {
            Array.Copy(targetL, gainL, nIr); Array.Copy(targetR, gainR, nIr);
            UpdateKernels();
        }
    }

    /// <summary>Blends the direct-form heads with the current gains (reversed, for the dot product).</summary>
    void UpdateKernels()
    {
        Array.Clear(kernL); Array.Clear(kernR);
        for (int i = 0; i < nIr; i++)
        {
            float gl = gainL[i], gr = gainR[i];
            if (gl != 0) for (int k = 0; k < B; k++) kernL[B - 1 - k] += gl * head[i * B + k];
            if (gr != 0) for (int k = 0; k < B; k++) kernR[B - 1 - k] += gr * head[i * B + k];
        }
    }

    /// <summary>A block of input is complete: compute the next block's contribution of every later partition.</summary>
    void EndBlock()
    {
        snap = false;
        for (int i = 0; i < nIr; i++)
        {
            gainL[i] += smooth * (targetL[i] - gainL[i]);
            if (Math.Abs(gainL[i] - targetL[i]) < 1e-5f) gainL[i] = targetL[i];
            gainR[i] += smooth * (targetR[i] - gainR[i]);
            if (Math.Abs(gainR[i] - targetR[i]) < 1e-5f) gainR[i] = targetR[i];
        }
        UpdateKernels();
        int n = 2 * B;
        if (parts > 0)
        {
            // spectrum of [previous block, this block] (overlap-save)
            Array.Copy(frame, fRe, n);
            Array.Clear(fIm);
            fft.Run(fRe, fIm, false);
            xPos = (xPos + 1) % parts;
            int o = xPos * Bins;
            for (int k = 0; k < Bins; k++) { xRe[o + k] = (float)fRe[k]; xIm[o + k] = (float)fIm[k]; }
            Array.Clear(accLRe); Array.Clear(accLIm); Array.Clear(accRRe); Array.Clear(accRIm);
            for (int i = 0; i < nIr; i++)
            {
                float gl = gainL[i], gr = gainR[i];
                if (gl == 0 && gr == 0) continue;
                Array.Clear(tRe); Array.Clear(tIm);
                for (int p = 0; p < parts; p++)
                {
                    // partition p+1 meets the input spectrum from p blocks before the newest
                    int xo = ((xPos + parts - p) % parts) * Bins;
                    int ho = (i * parts + p) * Bins;
                    for (int k = 0; k < Bins; k++)
                    {
                        float hr = hRe[ho + k], hi = hIm[ho + k], xr = xRe[xo + k], xi = xIm[xo + k];
                        tRe[k] += hr * xr - hi * xi;
                        tIm[k] += hr * xi + hi * xr;
                    }
                }
                if (gl != 0) for (int k = 0; k < Bins; k++) { accLRe[k] += gl * tRe[k]; accLIm[k] += gl * tIm[k]; }
                if (gr != 0) for (int k = 0; k < Bins; k++) { accRRe[k] += gr * tRe[k]; accRIm[k] += gr * tIm[k]; }
            }
            Inverse(accLRe, accLIm, tailL);
            Inverse(accRRe, accRIm, tailR);
        }
        Array.Copy(frame, B, frame, 0, B);
    }

    void Inverse(double[] ar, double[] ai, float[] tail)
    {
        int n = 2 * B;
        for (int k = 0; k < Bins; k++) { fRe[k] = ar[k]; fIm[k] = ai[k]; }
        for (int k = 1; k < B; k++) { fRe[n - k] = ar[k]; fIm[n - k] = -ai[k]; }
        fft.Run(fRe, fIm, true);
        for (int k = 0; k < B; k++) tail[k] = (float)(fRe[B + k] / n);
    }

    /// <summary>Mono in, stereo out.</summary>
    internal void Process(ReadOnlySpan<float> input, Span<float> l, Span<float> r)
    {
        for (int i = 0; i < input.Length; i++)
        {
            float x = input[i];
            hp = (hp + 1) % B;
            hist[hp] = x; hist[hp + B] = x;
            var h = new ReadOnlySpan<float>(hist, hp + 1, B);
            float al = 0, ar = 0;
            for (int k = 0; k < B; k++) { al += h[k] * kernL[k]; ar += h[k] * kernR[k]; }
            l[i] = al + tailL[pos];
            r[i] = ar + tailR[pos];
            frame[B + pos] = x;
            if (++pos == B) { pos = 0; EndBlock(); }
        }
    }
}

/// <summary>Cabinet simulation: a zero-latency partitioned convolver that runs several IRs at once into a
/// stereo pair, plus IR utilities (normalizing, resampling). The built-in cabs are in <see cref="Cab"/>.</summary>
public sealed class Convolver
{
    /// <summary>Longest IR kept, in samples (user IRs are truncated here).</summary>
    public const int MaxLength = 16384;
    PreparedIr ir = PreparedIr.Single(48000, new[] { 1f });
    readonly object gate = new();

    /// <summary>Swaps in a new IR set (built off the audio thread); returns the old one.</summary>
    public PreparedIr Swap(PreparedIr next) { lock (gate) { var old = ir; ir = next; return old; } }

    /// <summary>True when the mic position is part of the IRs (built-in cabs) rather than an EQ after them.</summary>
    public bool Positional => ir.Positional;

    /// <summary>Mic position for the built-in cabs (0 = cap .. 1 = cone), crossfaded smoothly.</summary>
    public void SetMic(float p) { lock (gate) ir.SetMic(p); }

    /// <summary>Mono in, stereo out.</summary>
    public void Process(ReadOnlySpan<float> input, Span<float> l, Span<float> r) { lock (gate) ir.Process(input, l, r); }

    /// <summary>Peak gain and mean power gain of an IR between 100 Hz and 6 kHz (log-spaced).</summary>
    internal static (double Peak, double Power) BandStats(float[] ir, double fs)
    {
        double peak = 1e-9, sum = 0, count = 0;
        int n = Math.Min(ir.Length, MaxLength);
        for (double f = 100; f < 6000; f *= 1.06)
        {
            double re = 0, im = 0, w = 2 * Math.PI * f / fs;
            for (int i = 0; i < n; i++) { re += ir[i] * Math.Cos(w * i); im -= ir[i] * Math.Sin(w * i); }
            double p = re * re + im * im;
            peak = Math.Max(peak, Math.Sqrt(p));
            sum += p; count++;
        }
        return (peak, sum / count);
    }

    /// <summary>Scales an IR so its loudest band (100 Hz–6 kHz) sits at unity gain.</summary>
    public static void Normalize(float[] ir, double fs)
    {
        float g = (float)(1 / BandStats(ir, fs).Peak);
        for (int i = 0; i < ir.Length; i++) ir[i] *= g;
    }

    /// <summary>Windowed-sinc resampler (Blackman window, 24 zero crossings each side, anti-aliased when
    /// downsampling).</summary>
    public static float[] Resample(float[] x, int from, int to)
    {
        if (from == to || x.Length == 0 || from <= 0 || to <= 0) return (float[])x.Clone();
        int n = (int)((long)x.Length * to / from);
        double step = (double)from / to;
        double cut = Math.Min((double)to / from, 1.0) * 0.96;
        double hw = 24.0 / cut;
        int last = x.Length - 1;
        var y = new float[n];
        for (int i = 0; i < n; i++)
        {
            double t = i * step;
            int a = (int)Math.Ceiling(t - hw), b = (int)Math.Floor(t + hw);
            double acc = 0;
            for (int k = Math.Max(a, 0); k <= Math.Min(b, last); k++)
            {
                double d = t - k, u = d / hw;
                double win = 0.42 + 0.5 * Math.Cos(Math.PI * u) + 0.08 * Math.Cos(2 * Math.PI * u);
                double s = Math.Abs(d) < 1e-9 ? 1 : Math.Sin(Math.PI * cut * d) / (Math.PI * cut * d);
                acc += x[k] * cut * s * win;
            }
            y[i] = (float)acc;
        }
        return y;
    }

    public static readonly string[] Models = { "4x12 V30", "2x12 ALNICO", "1x12 OPEN" };
}

/// <summary>
/// The built-in cabs, designed in the frequency domain: the original voicing curve, the mic's EQ at
/// that position, and dozens of narrow cone-breakup peaks and dips whose depth and frequency shift with
/// the mic position. Each response is made minimum-phase, then gets filtered cabinet reflections and a
/// short diffuse tail. Every cab is two speakers (L/R) at four mic positions, each blended with a
/// ribbon mic, so a cab is 10 IRs.
/// </summary>
public static class Cab
{
    /// <summary>Mic positions each built-in cab is designed at, evenly spaced from cap (0) to cone (1).</summary>
    public const int Positions = 4;
    /// <summary>Share of the ribbon mic in the built-in cabs' dual-mic blend.</summary>
    public const float RibbonMix = 0.3f;
    /// <summary>Length of the built-in cab IRs, in seconds.</summary>
    const double BuiltinSeconds = 0.045;

    /// <summary>Small deterministic RNG (splitmix64) so the built-in cabs sound the same every time.</summary>
    sealed class Rng
    {
        ulong s;
        public Rng(ulong seed) => s = seed;
        public double Next()
        {
            unchecked
            {
                s += 0x9E3779B97F4A7C15UL;
                ulong z = s;
                z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9UL;
                z = (z ^ (z >> 27)) * 0x94D049BB133111EBUL;
                return (z ^ (z >> 31)) / (double)ulong.MaxValue;
            }
        }
        /// <summary>Uniform in -1..1.</summary>
        public double Bi() => Next() * 2 - 1;
    }

    /// <summary>Each cab's voicing: the tonal balance of the speaker in its cabinet (this was the whole cab
    /// before; the detail below is layered on top so presets keep their balance).</summary>
    static Biquad[] Voicing(double fs, int model)
    {
        var f = new Biquad[8];
        switch (model)
        {
            case 1: // 2x12 alnico: looser lows, smooth mids, chimey top
                f[0].HighPass(fs, 85, 0.7); f[1].Peak(fs, 105, 1.2, 3); f[2].Peak(fs, 600, 0.9, -1.5);
                f[3].Peak(fs, 1600, 1.5, 1); f[4].Peak(fs, 3200, 1.4, 4.5); f[5].Peak(fs, 5200, 3, 1.5);
                f[6].LowPass(fs, 6600, 0.75); f[7].LowPass(fs, 8000, 0.6);
                break;
            case 2: // 1x12 open back: lighter bass, airy, upper-mid bark
                f[0].HighPass(fs, 100, 0.6); f[1].Peak(fs, 130, 1.0, 2); f[2].Peak(fs, 480, 1.2, -2.5);
                f[3].Peak(fs, 1100, 1.4, 2); f[4].Peak(fs, 2200, 1.6, 3.5); f[5].Peak(fs, 4300, 2.5, 2);
                f[6].LowPass(fs, 6000, 0.7); f[7].LowPass(fs, 7500, 0.6);
                break;
            default: // 4x12 V30: tight thump, mid scoop, aggressive upper mids
                f[0].HighPass(fs, 70, 0.8); f[1].Peak(fs, 115, 1.4, 5); f[2].Peak(fs, 420, 1.0, -4);
                f[3].Peak(fs, 1300, 2.0, 1.5); f[4].Peak(fs, 2600, 1.6, 5); f[5].Peak(fs, 4100, 3.0, 2.5);
                f[6].LowPass(fs, 5200, 0.75); f[7].LowPass(fs, 6500, 0.6);
                break;
        }
        return f;
    }

    /// <summary>One cone-breakup resonance (or dip, when Amp is negative). K is how fast the mode changes
    /// as the mic moves across the cone, and Ph where it starts.</summary>
    readonly record struct Mode(double F, double Q, double Amp, double K, double Ph);

    /// <summary>The cone's breakup modes as heard by one speaker. <paramref name="jitter"/> is how much this speaker differs from
    /// the cab's other speakers (0 = identical).</summary>
    static List<Mode> BreakupModes(double fs, int model, int speaker, double jitter)
    {
        var baseRng = new Rng(0x5EEDCAB0UL + (ulong)model * 7919);
        var spk = new Rng(0xC0FFEE00UL ^ ((ulong)model * 131 + (ulong)speaker * 977 + 1));
        double fLo = 1100, fHi = Math.Min(9500, 0.42 * fs);
        const int M = 44;
        var modes = new List<Mode>(M);
        for (int i = 0; i < M; i++)
        {
            double t = (i + 0.15 + 0.7 * baseRng.Next()) / M;
            double f = fLo * Math.Pow(fHi / fLo, t);
            double q = 5 + 13 * baseRng.Next();
            double env = 2 + 5 * (t * t * (3 - 2 * t)); // breakup grows toward the top
            double amp = env * (0.35 + 0.65 * baseRng.Next());
            if (baseRng.Next() < 0.4) amp *= -1.3; // cancellations dip deeper than resonances peak
            double k = 0.6 + 2.4 * t;
            double ph = 2 * baseRng.Next();
            double mf = f * (1 + jitter * 0.08 * spk.Bi());
            double ma = amp * (1 + jitter * 0.6 * spk.Bi());
            double mph = ph + jitter * 0.6 * spk.Bi();
            modes.Add(new Mode(mf, q, ma, k, mph));
        }
        return modes;
    }

    /// <summary>The filters for one mic on one speaker. <paramref name="pos"/> is the mic position (0 = cap .. 1 = cone).</summary>
    static List<Biquad> MicFilters(double fs, int model, int speaker, double pos, bool ribbon)
    {
        double jitter = model == 2 ? 0.45 : 1.0; // the 1x12's "two speakers" are one speaker, two mics
        var v = new List<Biquad>(Voicing(fs, model));
        Biquad b;
        // speaker-to-speaker variation in the low resonance and the midrange
        var spk = new Rng(0xA11CE000UL ^ ((ulong)model * 31 + (ulong)speaker * 7 + 3));
        double fr = 110 * (1 + 0.06 * jitter * spk.Bi()), gr = 1.0 * jitter * spk.Bi();
        double fm = 700 + 700 * spk.Next(), gm = 1.5 * jitter * spk.Bi();
        b = default; b.Peak(fs, fr, 2.0, gr); v.Add(b);
        b = default; b.Peak(fs, fm, 1.5, gm); v.Add(b);
        double scale;
        if (ribbon)
        {
            // a ribbon a little further back: smooth top, fuller lows, sees the cone more evenly
            b = default; b.LowShelf(fs, 160, 2.0); v.Add(b);
            b = default; b.HighShelf(fs, 3500, -4.5); v.Add(b);
            b = default; b.LowPass(fs, 8500, 0.6); v.Add(b);
            pos = 0.5; scale = 0.55;
        }
        else
        {
            // the dynamic mic's EQ at this position: cap = bright, cone = dark
            b = default; b.HighShelf(fs, 2500, 2.5 - 10 * pos); v.Add(b);
            b = default; b.LowPass(fs, Math.Min(16000 - 9000 * pos, 0.45 * fs), 0.7); v.Add(b);
            scale = 1.0;
        }
        foreach (var m in BreakupModes(fs, model, speaker, jitter))
        {
            double a = Math.PI * (m.K * pos + m.Ph);
            double f = Math.Min(m.F * (1 + 0.025 * Math.Sin(a)), 0.45 * fs);
            double g = scale * m.Amp * (0.55 + 0.45 * Math.Cos(a));
            b = default; b.Peak(fs, f, m.Q, g); v.Add(b);
        }
        return v;
    }

    /// <summary>The minimum-phase impulse response with the magnitude of <paramref name="filters"/>, <paramref name="len"/> samples long.</summary>
    static double[] MinPhaseIr(double fs, List<Biquad> filters, int len)
    {
        int n = Math.Max((int)System.Numerics.BitOperations.RoundUpToPowerOf2((uint)Math.Max((int)(fs * 0.25), 1)), 8192);
        var fft = new Fft(n);
        var re = new double[n]; var im = new double[n];
        // log magnitude (natural log), mirrored
        for (int k = 0; k <= n / 2; k++)
        {
            double w = 2 * Math.PI * k / n, c1 = Math.Cos(w), s1 = Math.Sin(w), c2 = Math.Cos(2 * w), s2 = Math.Sin(2 * w);
            double p = 1;
            foreach (var bq in filters) p *= bq.PowerAt(c1, s1, c2, s2);
            re[k] = 0.5 * Math.Log(Math.Max(p, 1e-12));
            if (k > 0 && k < n / 2) re[n - k] = re[k];
        }
        // real cepstrum, folded onto positive quefrencies
        fft.Run(re, im, true);
        for (int k = 0; k < n; k++) { re[k] /= n; im[k] = 0; }
        for (int k = 1; k < n / 2; k++) re[k] *= 2;
        for (int k = n / 2 + 1; k < n; k++) re[k] = 0;
        fft.Run(re, im, false);
        for (int k = 0; k < n; k++)
        {
            double m = Math.Exp(re[k]);
            (re[k], im[k]) = (m * Math.Cos(im[k]), m * Math.Sin(im[k]));
        }
        fft.Run(re, im, true);
        var h = new double[len];
        for (int k = 0; k < len; k++) h[k] = re[k] / n;
        return h;
    }

    /// <summary>Cabinet reflections (dulled by the box) and a short diffuse tail, seeded per IR.</summary>
    static float[] AddCabinet(double fs, int model, double[] h, ulong seed)
    {
        int len = h.Length;
        var o = (double[])h.Clone();
        void Refl(double delayS, double gain, double lpHz)
        {
            int d = (int)(fs * delayS);
            var lp = new Biquad();
            lp.LowPass(fs, lpHz, 0.6);
            for (int i = 0; i < len - d; i++) o[i + d] += gain * lp.Process(h[i]);
        }
        Refl(0.00055, 0.18, 3500);
        Refl(0.0013, -0.10, 2500);
        if (model == 2) Refl(0.0024, 0.14, 1800); // open back: the wall behind the cab
        double peak = 0;
        foreach (var v in h) peak = Math.Max(peak, Math.Abs(v));
        var rng = new Rng(seed);
        var lpT = new Biquad();
        lpT.LowPass(fs, 3500, 0.6);
        double t0 = fs * 0.0006, tau = fs * 0.005;
        for (int i = 0; i < len; i++)
        {
            double env = i < t0 ? i / t0 : Math.Exp(-(i - t0) / tau);
            o[i] += lpT.Process(0.03 * peak * env * rng.Bi());
        }
        return Array.ConvertAll(o, v => (float)v);
    }

    /// <summary>Mean power (100 Hz–6 kHz) of the old cab: voicing plus the mic EQ at the default position,
    /// normalized to a unity peak.</summary>
    static double ReferencePower(double fs, int model)
    {
        var v = new List<Biquad>(Voicing(fs, model));
        Biquad a = default, b = default;
        a.HighShelf(fs, 2500, -0.5);
        b.LowPass(fs, 13300, 0.7);
        v.Add(a); v.Add(b);
        double peak = 1e-9, sum = 0, count = 0;
        for (double f = 100; f < 6000; f *= 1.06)
        {
            double w = 2 * Math.PI * f / fs, c1 = Math.Cos(w), s1 = Math.Sin(w), c2 = Math.Cos(2 * w), s2 = Math.Sin(2 * w);
            double p = 1;
            foreach (var q in v) p *= q.PowerAt(c1, s1, c2, s2);
            peak = Math.Max(peak, p);
            sum += p; count++;
        }
        return sum / count / peak;
    }

    /// <summary>The 10 IRs of a built-in cab, in the order PreparedIr.PositionalSet expects.</summary>
    static List<float[]> Builtin(double fs, int model)
    {
        int len = (int)(fs * BuiltinSeconds);
        float[] One(int speaker, double pos, bool ribbon, ulong seed)
        {
            var h = MinPhaseIr(fs, MicFilters(fs, model, speaker, pos, ribbon), len);
            return AddCabinet(fs, model, h, seed ^ ((ulong)model * 0x1000 + (ulong)speaker * 0x100));
        }
        var irs = new List<float[]>(Positions * 2 + 2);
        for (int k = 0; k < Positions; k++)
        {
            double pos = (double)k / (Positions - 1);
            for (int s = 0; s < 2; s++) irs.Add(One(s, pos, false, 0xD1A0UL + (ulong)k));
        }
        for (int s = 0; s < 2; s++) irs.Add(One(s, 0.5, true, 0x41BB));
        // one gain for the whole cab: the default mic position's blend gets the average loudness the
        // old (voicing-only) cab had, so presets keep their level
        var d0 = irs[2]; var r0 = irs[2 * Positions];
        var mix = new float[Math.Min(d0.Length, r0.Length)];
        for (int i = 0; i < mix.Length; i++) mix[i] = d0[i] * (1 - RibbonMix) + r0[i] * RibbonMix;
        // (the trim takes back what the breakup peaks add on real playing, measured on the presets)
        double[] trim = { 0.87, 0.79, 0.785 };
        float g = (float)(trim[model] * Math.Sqrt(ReferencePower(fs, model) / Convolver.BandStats(mix, fs).Power));
        foreach (var ir in irs) for (int i = 0; i < ir.Length; i++) ir[i] *= g;
        return irs;
    }

    /// <summary>Builds the IR set for a cab model: 0..2 built-in, 3 = the user IR (falls back to model 0).</summary>
    public static PreparedIr Build(double fs, int model, float[] userData, int userRate)
    {
        if (model == 3 && userData != null)
        {
            var ir = Convolver.Resample(userData, userRate, (int)fs);
            if (ir.Length > Convolver.MaxLength) Array.Resize(ref ir, Convolver.MaxLength);
            Convolver.Normalize(ir, fs);
            return PreparedIr.Single(fs, ir);
        }
        return PreparedIr.PositionalSet(fs, Builtin(fs, Math.Clamp(model, 0, 2)));
    }
}
