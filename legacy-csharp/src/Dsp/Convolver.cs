using System;
using System.Numerics;

namespace Jwala.Dsp;

/// <summary>
/// Zero-latency direct-form FIR convolver (SIMD) for cabinet impulse responses.
/// IRs are truncated to MaxLength, which covers typical guitar cab IRs.
/// </summary>
public sealed class Convolver
{
    public const int MaxLength = 2048;
    float[] irRev = new float[1];
    float[] hist = new float[2];
    int n = 1, pos;

    public void SetImpulse(float[] ir)
    {
        int len = Math.Min(ir.Length, MaxLength);
        // round up to the SIMD width so the dot product has no tail loop
        int w = Vector<float>.Count;
        int padded = (len + w - 1) / w * w;
        var rev = new float[padded];
        for (int i = 0; i < len; i++)
        {
            float fade = i > len - 64 ? (len - i) / 64f : 1f; // avoid truncation click
            rev[padded - 1 - i] = ir[i] * fade;
        }
        var newHist = new float[padded * 2];
        // swap atomically-ish: audio thread reads the trio below via locals
        lock (this) { irRev = rev; hist = newHist; n = padded; pos = 0; }
    }

    public void Process(Span<float> buf)
    {
        lock (this)
        {
            int w = Vector<float>.Count;
            for (int s = 0; s < buf.Length; s++)
            {
                pos = (pos + 1) % n;
                hist[pos] = buf[s]; hist[pos + n] = buf[s];
                var acc = Vector<float>.Zero;
                var hs = new ReadOnlySpan<float>(hist, pos + 1, n);
                var ks = new ReadOnlySpan<float>(irRev);
                for (int i = 0; i < n; i += w)
                    acc += new Vector<float>(hs.Slice(i)) * new Vector<float>(ks.Slice(i));
                buf[s] = Vector.Sum(acc);
            }
        }
    }

    /// <summary>Scales an IR so its loudest band (100 Hz–6 kHz) sits at unity gain.</summary>
    public static void Normalize(float[] ir, double fs)
    {
        double peak = 1e-9;
        for (double f = 100; f < 6000; f *= 1.06)
        {
            double re = 0, im = 0, w = 2 * Math.PI * f / fs;
            for (int i = 0; i < Math.Min(ir.Length, MaxLength); i++) { re += ir[i] * Math.Cos(w * i); im -= ir[i] * Math.Sin(w * i); }
            peak = Math.Max(peak, Math.Sqrt(re * re + im * im));
        }
        float g = (float)(1 / peak);
        for (int i = 0; i < ir.Length; i++) ir[i] *= g;
    }

    public static readonly string[] Models = { "4x12 V30", "2x12 ALNICO", "1x12 OPEN" };

    /// <summary>Built-in cabs: an impulse pushed through a speaker-shaped EQ plus early reflections.</summary>
    public static float[] SyntheticCab(double fs, int model = 0)
    {
        int len = (int)(fs * 0.03);
        var ir = new float[len];
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
        int d1 = (int)(fs * 0.00055), d2 = (int)(fs * 0.0013);
        for (int i = 0; i < len; i++)
        {
            double x = i == 0 ? 1 : 0;
            if (i == d1) x += 0.22;  // cabinet early reflections
            if (i == d2) x -= 0.12;
            for (int k = 0; k < f.Length; k++) x = f[k].Process(x);
            ir[i] = (float)x;
        }
        Normalize(ir, fs);
        return ir;
    }
}
