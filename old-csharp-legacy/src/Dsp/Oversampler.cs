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

    static double BesselI0(double x)
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
