using System;

namespace Jwala.Dsp;

/// <summary>
/// Polyphase FIR up/down sampler (factor 4, Kaiser-windowed sinc) so the nonlinear
/// stages run at 4x and their harmonics don't alias back down.
/// </summary>
public sealed class Oversampler
{
    public const int Factor = 4;
    const int Taps = 96; // multiple of Factor
    readonly double[] h = new double[Taps];
    readonly double[] upHist = new double[Taps / Factor * 2];
    readonly double[] downHist = new double[Taps * 2];
    int upPos, downPos;

    public Oversampler()
    {
        double fc = 0.45 / Factor; // cutoff relative to the oversampled rate
        double beta = 9.0, sum = 0;
        for (int i = 0; i < Taps; i++)
        {
            double n = i - (Taps - 1) / 2.0;
            double sinc = n == 0 ? 2 * fc : Math.Sin(2 * Math.PI * fc * n) / (Math.PI * n);
            double r = 2.0 * i / (Taps - 1) - 1;
            h[i] = sinc * BesselI0(beta * Math.Sqrt(1 - r * r)) / BesselI0(beta);
            sum += h[i];
        }
        for (int i = 0; i < Taps; i++) h[i] /= sum;
    }

    internal static double BesselI0(double x)
    {
        double s = 1, t = 1;
        for (int k = 1; k < 30; k++) { t *= x / (2 * k); s += t * t; }
        return s;
    }

    /// <summary>Takes one base-rate sample, writes Factor oversampled samples.</summary>
    public void Up(double x, Span<double> outp)
    {
        int len = Taps / Factor;
        upPos = (upPos + 1) % len;
        upHist[upPos] = x; upHist[upPos + len] = x;
        // newest sample is at upHist[upPos + len]; walk backwards in time
        for (int p = 0; p < Factor; p++)
        {
            double acc = 0;
            for (int j = 0; j < len; j++) acc += h[j * Factor + p] * upHist[upPos + len - j];
            outp[p] = acc * Factor;
        }
    }

    /// <summary>Takes Factor oversampled samples, returns one base-rate sample.</summary>
    public double Down(ReadOnlySpan<double> inp)
    {
        for (int p = 0; p < Factor; p++)
        {
            downPos = (downPos + 1) % Taps;
            downHist[downPos] = inp[p]; downHist[downPos + Taps] = inp[p];
        }
        double acc = 0;
        for (int j = 0; j < Taps; j++) acc += h[j] * downHist[downPos + Taps - j];
        return acc;
    }
}

/// <summary>
/// A further 2x on top of the 4x region, for the LEAD II amp only (its five cascaded stages
/// make far more harmonics than the other channels). Half-band FIR, 23 taps: every other tap
/// is zero and the centre is one half, so it costs twelve multiplies per sample pair. The signal
/// it carries is already band-limited by the 4x stage, which is why so short a filter is enough.
/// </summary>
public sealed class HalfBandDoubler
{
    /// <summary>Number of non-zero taps either side of the centre, counted together.</summary>
    const int HB = 12;
    /// <summary>The taps at odd offsets from the centre.</summary>
    readonly double[] h = new double[HB];
    readonly double[] upHist = new double[HB];
    readonly double[] downHist = new double[2 * HB - 1];

    public HalfBandDoubler()
    {
        double beta = 8.2, sum = 0;
        for (int j = 0; j < HB; j++)
        {
            double o = 2.0 * j - (HB - 1), r = o / (HB - 1);
            h[j] = Math.Sin(Math.PI * o / 2) / (Math.PI * o) * Oversampler.BesselI0(beta * Math.Sqrt(1 - r * r)) / Oversampler.BesselI0(beta);
            sum += h[j];
        }
        // with the centre tap at one half, the rest must add up to the other half
        for (int j = 0; j < HB; j++) h[j] *= 0.5 / sum;
    }

    public void Reset() { Array.Clear(upHist); Array.Clear(downHist); }

    /// <summary>Takes one sample, returns two at twice the rate (earlier first).</summary>
    public (double, double) Up(double x)
    {
        Array.Copy(upHist, 0, upHist, 1, HB - 1);
        upHist[0] = x;
        double acc = 0;
        for (int j = 0; j < HB; j++) acc += h[j] * upHist[j];
        return (2 * acc, upHist[HB / 2 - 1]);
    }

    /// <summary>Takes two samples (earlier first), returns one at half the rate.</summary>
    public double Down(double a, double b)
    {
        Array.Copy(downHist, 0, downHist, 2, 2 * HB - 3);
        downHist[1] = a; downHist[0] = b;
        double acc = 0.5 * downHist[HB - 1];
        for (int j = 0; j < HB; j++) acc += h[j] * downHist[2 * j];
        return acc;
    }
}
