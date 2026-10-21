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

    void Set(double nb0, double nb1, double nb2, double na0, double na1, double na2)
    {
        b0 = nb0 / na0; b1 = nb1 / na0; b2 = nb2 / na0; a1 = na1 / na0; a2 = na2 / na0;
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
    // JCM800-ish component values
    const double R1 = 220e3, R2 = 1e6, R3 = 22e3, R4 = 33e3, C1 = 470e-12, C2 = 22e-9, C3 = 22e-9;
    readonly double fs;
    double B0, B1, B2, B3, A1, A2, A3;
    double x1, x2, x3, y1, y2, y3;
    double lt = -1, lm = -1, ll = -1;

    public ToneStack(double sampleRate) { fs = sampleRate; Set(0.5, 0.5, 0.5); }

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
