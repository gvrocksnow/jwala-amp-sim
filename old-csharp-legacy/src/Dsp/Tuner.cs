using System;

namespace Jwala.Dsp;

/// <summary>
/// Chromatic tuner. The audio thread feeds raw input into a ring buffer; the UI thread
/// periodically runs YIN pitch detection on the most recent window.
/// </summary>
public sealed class Tuner
{
    const int RingSize = 16384;
    readonly float[] ring = new float[RingSize];
    volatile int writePos;
    public volatile bool Active;
    public volatile bool MuteOutput = true;
    public double ReferenceA = 440;

    // results (UI thread)
    public double Frequency { get; private set; }
    public bool HasSignal { get; private set; }
    readonly double[] recent = new double[5]; int recentCount, recentIdx;
    double smoothed;

    const int Decim = 2, Window = 1024, MinHz = 50, MaxHz = 1200;
    readonly float[] buf = new float[Window * 2 + 8];
    readonly double[] d = new double[Window];

    /// <summary>Audio thread: append input samples (only while the tuner is open).</summary>
    public void Write(ReadOnlySpan<float> x)
    {
        if (!Active) return;
        int w = writePos;
        for (int i = 0; i < x.Length; i++) { ring[w] = x[i]; w = (w + 1) & (RingSize - 1); }
        writePos = w;
    }

    /// <summary>UI thread: analyse the latest audio. Returns true when a stable pitch was found.</summary>
    public bool Analyze(double sampleRate)
    {
        // pull the newest 2*Window decimated samples (simple 2-tap average as anti-alias)
        int need = buf.Length * Decim, start = (writePos - need) & (RingSize - 1);
        double sum2 = 0;
        for (int i = 0; i < buf.Length; i++)
        {
            int k = (start + i * Decim) & (RingSize - 1);
            float v = 0.5f * (ring[k] + ring[(k + 1) & (RingSize - 1)]);
            buf[i] = v; sum2 += v * v;
        }
        double rms = Math.Sqrt(sum2 / buf.Length);
        if (rms < 0.002) { Lose(); return false; } // ~ -54 dBFS: no note

        double fs = sampleRate / Decim;
        int tauMin = (int)(fs / MaxHz), tauMax = Math.Min(Window - 1, (int)(fs / MinHz));

        // YIN: difference function + cumulative mean normalisation
        d[0] = 1; double running = 0; int tau = -1;
        for (int t = 1; t <= tauMax; t++)
        {
            double s = 0;
            for (int j = 0; j < Window; j++) { double diff = buf[j] - buf[j + t]; s += diff * diff; }
            running += s;
            d[t] = running > 0 ? s * t / running : 1;
        }
        for (int t = tauMin; t <= tauMax; t++)
        {
            if (d[t] < 0.12)
            {
                while (t + 1 <= tauMax && d[t + 1] < d[t]) t++;
                tau = t; break;
            }
        }
        if (tau < 0) { Lose(); return false; }

        // parabolic interpolation around the minimum
        double betterTau = tau;
        if (tau > 1 && tau < tauMax)
        {
            double a = d[tau - 1], b = d[tau], c = d[tau + 1], den = a + c - 2 * b;
            if (Math.Abs(den) > 1e-12) betterTau = tau + 0.5 * (a - c) / den;
        }
        double f = fs / betterTau;

        // median of the last few readings rejects octave blips; then light smoothing
        recent[recentIdx] = f; recentIdx = (recentIdx + 1) % recent.Length; recentCount = Math.Min(recentCount + 1, recent.Length);
        var sorted = new double[recentCount]; Array.Copy(recent, sorted, recentCount); Array.Sort(sorted);
        double med = sorted[recentCount / 2];
        if (!HasSignal || Math.Abs(1200 * Math.Log2(med / smoothed)) > 50) smoothed = med;
        else smoothed += 0.35 * (med - smoothed);
        Frequency = smoothed; HasSignal = true;
        return true;
    }

    void Lose() { HasSignal = false; recentCount = 0; }

    static readonly string[] Names = { "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B" };

    /// <summary>Nearest note name, octave and deviation in cents for a frequency.</summary>
    public (string Note, int Octave, double Cents) Describe(double f)
    {
        double midi = 69 + 12 * Math.Log2(f / ReferenceA);
        int n = (int)Math.Round(midi);
        return (Names[((n % 12) + 12) % 12], n / 12 - 1, (midi - n) * 100);
    }
}
