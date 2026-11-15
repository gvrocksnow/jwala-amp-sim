using System;

namespace Jwala.Dsp;

/// <summary>RBJ cookbook biquad, transposed direct form II, double precision.</summary>
public struct Biquad
{
    double b0, b1, b2, a1, a2, z1, z2;

    public double Process(double x)
    {
        double y = b0 * x + z1;
        z1 = b1 * x - a1 * y + z2;
        z2 = b2 * x - a2 * y;
        return y;
    }

    public void Reset() => z1 = z2 = 0;

    /// <summary>Takes another biquad's coefficients and keeps this one's state.</summary>
    public void CopyCoefs(in Biquad o) { b0 = o.b0; b1 = o.b1; b2 = o.b2; a1 = o.a1; a2 = o.a2; }

    /// <summary>Gain in dB at <paramref name="f"/> Hz (for drawing response curves).</summary>
    public readonly double MagnitudeDb(double fs, double f)
    {
        double w = 2 * Math.PI * f / fs, c1 = Math.Cos(w), s1 = Math.Sin(w), c2 = Math.Cos(2 * w), s2 = Math.Sin(2 * w);
        double nr = b0 + b1 * c1 + b2 * c2, ni = -(b1 * s1 + b2 * s2);
        double dr = 1 + a1 * c1 + a2 * c2, di = -(a1 * s1 + a2 * s2);
        return 10 * Math.Log10(Math.Max((nr * nr + ni * ni) / Math.Max(dr * dr + di * di, 1e-30), 1e-30));
    }

    /// <summary>Power gain at a frequency given as cos w, sin w, cos 2w, sin 2w (for designing IRs fast).</summary>
    public readonly double PowerAt(double c1, double s1, double c2, double s2)
    {
        double nr = b0 + b1 * c1 + b2 * c2, ni = -(b1 * s1 + b2 * s2);
        double dr = 1 + a1 * c1 + a2 * c2, di = -(a1 * s1 + a2 * s2);
        return (nr * nr + ni * ni) / Math.Max(dr * dr + di * di, 1e-30);
    }

    void Set(double nb0, double nb1, double nb2, double na0, double na1, double na2)
    {
        b0 = nb0 / na0; b1 = nb1 / na0; b2 = nb2 / na0; a1 = na1 / na0; a2 = na2 / na0;
    }

    /// <summary>Bilinear transform of the analog section (b0 + b1 s + b2 s^2) / (a0 + a1 s + a2 s^2).</summary>
    public void Analog(double fs, double ab0, double ab1, double ab2, double aa0, double aa1, double aa2)
    {
        double k = 2 * fs, kk = k * k;
        Set(ab0 + ab1 * k + ab2 * kk, 2 * (ab0 - ab2 * kk), ab0 - ab1 * k + ab2 * kk,
            aa0 + aa1 * k + aa2 * kk, 2 * (aa0 - aa2 * kk), aa0 - aa1 * k + aa2 * kk);
    }

    public void LowPass(double fs, double f, double q)
    {
        double w = 2 * Math.PI * f / fs, c = Math.Cos(w), al = Math.Sin(w) / (2 * q);
        Set((1 - c) / 2, 1 - c, (1 - c) / 2, 1 + al, -2 * c, 1 - al);
    }

    public void HighPass(double fs, double f, double q)
    {
        double w = 2 * Math.PI * f / fs, c = Math.Cos(w), al = Math.Sin(w) / (2 * q);
        Set((1 + c) / 2, -(1 + c), (1 + c) / 2, 1 + al, -2 * c, 1 - al);
    }

    /// <summary>Band-pass with 0 dB gain at the centre.</summary>
    public void BandPass(double fs, double f, double q)
    {
        double w = 2 * Math.PI * f / fs, c = Math.Cos(w), al = Math.Sin(w) / (2 * q);
        Set(al, 0, -al, 1 + al, -2 * c, 1 - al);
    }

    public void Peak(double fs, double f, double q, double db)
    {
        double A = Math.Pow(10, db / 40), w = 2 * Math.PI * f / fs, c = Math.Cos(w), al = Math.Sin(w) / (2 * q);
        Set(1 + al * A, -2 * c, 1 - al * A, 1 + al / A, -2 * c, 1 - al / A);
    }

    /// <summary>
    /// Peaking bell matched to the analog prototype (Vicanek, "Matched Second Order Digital
    /// Filters", 2016): impulse-invariant poles, zeros solved so the gain is exactly <paramref name="db"/> at
    /// <paramref name="f"/> with the peak sitting there. Unlike the bilinear Peak, bells near Nyquist keep
    /// their analog width and shape instead of being squeezed, so the top bands stay open and airy.
    /// </summary>
    public void PeakMatched(double fs, double f, double q, double db)
    {
        double g = Math.Pow(10, db / 20), w = 2 * Math.PI * f / fs;
        // RBJ-symmetric prototype: pole Q = Q·√G, so boosts and cuts mirror each other
        double z = 1 / (2 * q * Math.Sqrt(g)), e = Math.Exp(-z * w);
        double na1 = z <= 1 ? -2 * e * Math.Cos(Math.Sqrt(1 - z * z) * w) : -2 * e * Math.Cosh(Math.Sqrt(z * z - 1) * w);
        double na2 = e * e;
        double ca0 = Sq(1 + na1 + na2), ca1 = Sq(1 - na1 + na2), ca2 = -4 * na2;
        double p1 = Sq(Math.Sin(w / 2)), p0 = 1 - p1, p2 = 4 * p0 * p1, g2 = g * g;
        double r1 = (ca0 * p0 + ca1 * p1 + ca2 * p2) * g2;
        double r2 = (-ca0 + ca1 + 4 * (p0 - p1) * ca2) * g2;
        double cb0 = ca0, cb2 = (r1 - r2 * p1 - cb0) / (4 * p1 * p1), cb1 = r2 + cb0 + 4 * (p1 - p0) * cb2;
        double s0 = Math.Sqrt(cb0), s1 = Math.Sqrt(Math.Max(cb1, 0)), wq = 0.5 * (s0 + s1);
        double nb0 = 0.5 * (wq + Math.Sqrt(Math.Max(wq * wq + cb2, 0))), nb1 = 0.5 * (s0 - s1), nb2 = -cb2 / (4 * nb0);
        Set(nb0, nb1, nb2, 1, na1, na2);
    }

    static double Sq(double x) => x * x;

    public void LowShelf(double fs, double f, double db)
    {
        double A = Math.Pow(10, db / 40), w = 2 * Math.PI * f / fs, c = Math.Cos(w), s = Math.Sin(w);
        double al = s / 2 * Math.Sqrt(2), sa = 2 * Math.Sqrt(A) * al;
        Set(A * ((A + 1) - (A - 1) * c + sa), 2 * A * ((A - 1) - (A + 1) * c), A * ((A + 1) - (A - 1) * c - sa),
            (A + 1) + (A - 1) * c + sa, -2 * ((A - 1) + (A + 1) * c), (A + 1) + (A - 1) * c - sa);
    }

    public void HighShelf(double fs, double f, double db)
    {
        double A = Math.Pow(10, db / 40), w = 2 * Math.PI * f / fs, c = Math.Cos(w), s = Math.Sin(w);
        double al = s / 2 * Math.Sqrt(2), sa = 2 * Math.Sqrt(A) * al;
        Set(A * ((A + 1) + (A - 1) * c + sa), -2 * A * ((A - 1) + (A + 1) * c), A * ((A + 1) + (A - 1) * c - sa),
            (A + 1) - (A - 1) * c + sa, 2 * ((A - 1) - (A + 1) * c), (A + 1) - (A - 1) * c - sa);
    }
}

/// <summary>One-pole low/high pass.</summary>
public struct OnePole
{
    double a, z;
    public void SetLowPass(double fs, double f) => a = 1 - Math.Exp(-2 * Math.PI * f / fs);
    public double LowPass(double x) => z += a * (x - z);
    public double HighPass(double x) => x - LowPass(x);
    public void Reset() => z = 0;
}

/// <summary>
/// Marshall-style TMB tone stack, discretized with the bilinear transform
/// (Yeh &amp; Smith, "Discretization of the '59 Fender Bassman Tone Stack", DAFx 2006).
/// </summary>
public sealed class ToneStack
{
    /// <summary>Component values as [treble pot, bass pot, mid pot, slope resistor, treble cap, bass cap,
    /// mid cap]. JCM800-ish: the clean, crunch and lead channels.</summary>
    public static readonly double[] JCM800 = { 220e3, 1e6, 22e3, 33e3, 470e-12, 22e-9, 22e-9 };
    /// <summary>The SLO-family stack that goes with the LEAD II preamp: a larger slope resistor, so the
    /// mids scoop deeper.</summary>
    public static readonly double[] SLO = { 250e3, 1e6, 25e3, 47e3, 470e-12, 22e-9, 22e-9 };
    readonly double fs;
    readonly double R1, R2, R3, R4, C1, C2, C3;
    double B0, B1, B2, B3, A1, A2, A3;
    double x1, x2, x3, y1, y2, y3;
    double lt = -1, lm = -1, ll = -1;

    public ToneStack(double sampleRate) : this(sampleRate, JCM800) { }

    public ToneStack(double sampleRate, double[] parts)
    {
        fs = sampleRate;
        (R1, R2, R3, R4, C1, C2, C3) = (parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]);
        Set(0.5, 0.5, 0.5);
    }

    /// <param name="treble">0..1</param><param name="mid">0..1</param><param name="bass">0..1</param>
    public void Set(double treble, double mid, double bass)
    {
        if (treble == lt && mid == lm && bass == ll) return;
        lt = treble; lm = mid; ll = bass;
        double t = treble, m = mid, l = Math.Exp((bass - 1) * 3.5); // log taper bass pot

        double b1 = t * C1 * R1 + m * C3 * R3 + l * (C1 * R2 + C2 * R2) + (C1 * R3 + C2 * R3);
        double b2 = t * (C1 * C2 * R1 * R4 + C1 * C3 * R1 * R4) - m * m * (C1 * C3 * R3 * R3 + C2 * C3 * R3 * R3)
                  + m * (C1 * C3 * R1 * R3 + C1 * C3 * R3 * R3 + C2 * C3 * R3 * R3)
                  + l * (C1 * C2 * R1 * R2 + C1 * C2 * R2 * R4 + C1 * C3 * R2 * R4)
                  + l * m * (C1 * C3 * R2 * R3 + C2 * C3 * R2 * R3)
                  + (C1 * C2 * R1 * R3 + C1 * C2 * R3 * R4 + C1 * C3 * R3 * R4);
        double b3 = l * m * (C1 * C2 * C3 * R1 * R2 * R3 + C1 * C2 * C3 * R2 * R3 * R4)
                  - m * m * (C1 * C2 * C3 * R1 * R3 * R3 + C1 * C2 * C3 * R3 * R3 * R4)
                  + m * (C1 * C2 * C3 * R1 * R3 * R3 + C1 * C2 * C3 * R3 * R3 * R4)
                  + t * C1 * C2 * C3 * R1 * R3 * R4 - t * m * C1 * C2 * C3 * R1 * R3 * R4
                  + t * l * C1 * C2 * C3 * R1 * R2 * R4;
        double a0 = 1;
        double a1 = (C1 * R1 + C1 * R3 + C2 * R3 + C2 * R4 + C3 * R4) + m * C3 * R3 + l * (C1 * R2 + C2 * R2);
        double a2 = m * (C1 * C3 * R1 * R3 - C2 * C3 * R3 * R4 + C1 * C3 * R3 * R3 + C2 * C3 * R3 * R3)
                  + l * m * (C1 * C3 * R2 * R3 + C2 * C3 * R2 * R3)
                  - m * m * (C1 * C3 * R3 * R3 + C2 * C3 * R3 * R3)
                  + l * (C1 * C2 * R2 * R4 + C1 * C2 * R1 * R2 + C1 * C3 * R2 * R4 + C2 * C3 * R2 * R4)
                  + (C1 * C2 * R1 * R4 + C1 * C3 * R1 * R4 + C1 * C2 * R3 * R4 + C1 * C2 * R1 * R3 + C1 * C3 * R3 * R4 + C2 * C3 * R3 * R4);
        double a3 = l * m * (C1 * C2 * C3 * R1 * R2 * R3 + C1 * C2 * C3 * R2 * R3 * R4)
                  - m * m * (C1 * C2 * C3 * R1 * R3 * R3 + C1 * C2 * C3 * R3 * R3 * R4)
                  + m * (C1 * C2 * C3 * R3 * R3 * R4 + C1 * C2 * C3 * R1 * R3 * R3 - C1 * C2 * C3 * R1 * R3 * R4)
                  + l * C1 * C2 * C3 * R1 * R2 * R4 + C1 * C2 * C3 * R1 * R3 * R4;

        double c = 2 * fs, c2 = c * c, c3 = c2 * c;
        double nB0 = -b1 * c - b2 * c2 - b3 * c3;
        double nB1 = -b1 * c + b2 * c2 + 3 * b3 * c3;
        double nB2 = b1 * c + b2 * c2 - 3 * b3 * c3;
        double nB3 = b1 * c - b2 * c2 + b3 * c3;
        double nA0 = -a0 - a1 * c - a2 * c2 - a3 * c3;
        double nA1 = -3 * a0 - a1 * c + a2 * c2 + 3 * a3 * c3;
        double nA2 = -3 * a0 + a1 * c + a2 * c2 - 3 * a3 * c3;
        double nA3 = -a0 + a1 * c - a2 * c2 + a3 * c3;
        B0 = nB0 / nA0; B1 = nB1 / nA0; B2 = nB2 / nA0; B3 = nB3 / nA0;
        A1 = nA1 / nA0; A2 = nA2 / nA0; A3 = nA3 / nA0;
    }

    public double Process(double x)
    {
        double y = B0 * x + B1 * x1 + B2 * x2 + B3 * x3 - A1 * y1 - A2 * y2 - A3 * y3;
        x3 = x2; x2 = x1; x1 = x; y3 = y2; y2 = y1; y1 = y;
        return y;
    }
}
