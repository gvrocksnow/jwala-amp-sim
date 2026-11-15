using System;

namespace Jwala.Dsp;

/// <summary>
/// The LEAD II preamp: five cascaded 12AX7 stages modelled at circuit level, in real volts.
///
/// Each stage is a common-cathode triode (Koren plate-current model) with its plate load,
/// cathode resistor and bypass cap, and an input network of coupling cap, attenuator, grid
/// stopper and Miller capacitance. Grid current charges the coupling cap and plate current
/// charges the cathode cap, so blocking distortion, bias shift and recovery after a hard
/// pick come from the capacitor states rather than from an envelope follower.
///
/// The component values are those of the SLO-family overdrive preamp that the 5150's lead
/// channel descends from (partially bypassed 1.8k cathodes, a 39k cold clipper as the third
/// stage, large grid stoppers). They are a starting point for voicing, not a transcription
/// of one schematic.
///
/// Per sample each stage solves one scalar equation for the grid-to-cathode voltage. The
/// plate current along the stage's load line and the grid current are both functions of
/// that voltage alone, so they are tabulated once and the solve is a few table lookups.
/// </summary>
public sealed class HiGain
{
    // Grid-to-cathode range of the tables. Below it the valve is cut off (constant); above it
    // the tables extrapolate linearly (plate bottomed, grid conducting through its resistance).
    const double XMin = -12.0, XMax = 6.0;
    const int TabN = 4096;
    const double XStep = (XMax - XMin) / TabN;

    /// <summary>Peak DI sample level -> volts at the input jack (a hot humbucker is around 1 V peak).</summary>
    public const double InputVolts = 3.0;
    /// <summary>Plate swing of the last stage (volts) -> the level the tone stack and power amp expect.</summary>
    const double OutScale = 1.0 / 150.0;

    static double Softplus(double a) => a > 30 ? a : Math.Log(1 + Math.Exp(a));

    /// <summary>Koren's 12AX7 plate current in amps.</summary>
    static double Koren(double vpk, double vgk)
    {
        const double Mu = 100, Ex = 1.4, Kg1 = 1060, Kp = 600, Kvb = 300;
        if (vpk <= 0) return 0;
        double a = Kp * (1 / Mu + vgk / Math.Sqrt(Kvb + vpk * vpk));
        return 2 * Math.Pow(vpk / Kp * Softplus(a), Ex) / Kg1;
    }

    /// <summary>Grid current in amps: a soft-kneed diode that conducts as the grid goes positive.</summary>
    static double GridCurrent(double vgk)
    {
        const double Rgk = 2000, V0 = -0.1, S = 0.15;
        return S / Rgk * Softplus((vgk - V0) / S);
    }

    /// <summary>One stage's parts. Resistances in ohms, capacitances in farads; 0 means "not fitted".</summary>
    /// <param name="Vb">Supply.</param><param name="Rp">Plate load.</param><param name="Rk">Cathode resistor.</param>
    /// <param name="Ck">Cathode bypass cap.</param><param name="Ro">Source impedance.</param><param name="Cc">Coupling cap (0 = direct).</param>
    /// <param name="Rs">Series resistor.</param><param name="Rg">Shunt to ground.</param><param name="Rgs">Grid stopper.</param>
    /// <param name="Cm">Grid-to-ground (Miller) capacitance.</param><param name="PlateLp">Low-pass on the plate (a plate bypass cap), Hz; 0 = none.</param>
    readonly record struct Circuit(double Vb, double Rp, double Rk, double Ck, double Ro, double Cc, double Rs, double Rg, double Rgs, double Cm, double PlateLp);

    sealed class Stage
    {
        readonly Circuit c;
        // Backward-Euler companions: cathode network resistance, Ck/T, T/Cc, Cm/T.
        readonly double ak, ckT, ccT, cmT;
        // Input network as seen from the grid (see SetInput): 1 / rc, the source's share that
        // reaches the grid over the resistance it arrives through, and that resistance with the
        // Miller cap's companion in parallel.
        double invRc, kTh, rth2;
        // Operating point.
        readonly double ipQ, vkQ;
        // [plate current on the load line, grid current] against grid-to-cathode volts.
        readonly double[] tabL = new double[TabN + 1], tabG = new double[TabN + 1];
        // State: cathode, coupling-cap and grid voltages, last solution.
        double vk, vc, vg, x;
        OnePole lp;

        public Stage(double fs, Circuit c)
        {
            this.c = c;
            double t = 1 / fs;
            ckT = c.Ck / t;
            ak = 1 / (1 / c.Rk + ckT);

            double lo = 0, hi = c.Vb / (c.Rp + c.Rk);
            for (int k = 0; k < 60; k++)
            {
                double ip = 0.5 * (lo + hi);
                if (ip > Koren(c.Vb - (c.Rp + c.Rk) * ip, -c.Rk * ip)) hi = ip; else lo = ip;
            }
            vkQ = c.Rk * 0.5 * (lo + hi);
            double bkQ = vkQ * ak * ckT;

            double rl = c.Rp + ak, v0 = c.Vb - bkQ;
            for (int i = 0; i <= TabN; i++)
            {
                double xv = XMin + i * XStep;
                double l = 0, h = v0 / rl;
                for (int k = 0; k < 40; k++)
                {
                    double ip = 0.5 * (l + h);
                    if (ip > Koren(v0 - rl * ip, xv)) h = ip; else l = ip;
                }
                tabL[i] = 0.5 * (l + h);
                tabG[i] = GridCurrent(xv);
            }

            if (c.PlateLp > 0) lp.SetLowPass(fs, c.PlateLp);
            ccT = c.Cc > 0 ? t / c.Cc : 0;
            cmT = c.Cm / t;
            ipQ = Lookup(-vkQ, out _, out _, out _, out _);
            SetInput(c.Rs, c.Rg);
            Reset();
        }

        /// <summary>Sets the attenuator in front of the grid: <paramref name="rs"/> in series after the
        /// coupling cap, <paramref name="rg"/> from there to ground.</summary>
        public void SetInput(double rs, double rg)
        {
            double rc = c.Ro + rs + ccT;
            double rth = rc * rg / (rc + rg) + c.Rgs;
            invRc = 1 / rc;
            kTh = rg / (rc + rg) / rth;
            rth2 = 1 / (cmT + 1 / rth);
        }

        public void Reset() { vk = vkQ; vc = 0; vg = 0; x = -vkQ; lp.Reset(); }

        /// <summary>Which straight piece of the tables <paramref name="xv"/> falls on (-1 below them, where they are flat).</summary>
        static int Segment(double xv)
        {
            double p = (xv - XMin) * (1 / XStep);
            return p <= 0 ? -1 : (int)Math.Min((long)p, TabN - 1);
        }

        /// <summary>Plate current (returned), grid current, their slopes and the table segment at grid-to-cathode voltage <paramref name="xv"/>.</summary>
        double Lookup(double xv, out double g, out double dl, out double dg, out int seg)
        {
            double p = (xv - XMin) * (1 / XStep);
            if (p <= 0) { g = tabG[0]; dl = dg = 0; seg = -1; return tabL[0]; }
            int i = (int)Math.Min((long)p, TabN - 1);
            double f = p - i; // above the table this exceeds 1: linear extrapolation
            double el = tabL[i + 1] - tabL[i], eg = tabG[i + 1] - tabG[i];
            g = tabG[i] + f * eg; dl = el * (1 / XStep); dg = eg * (1 / XStep); seg = i;
            return tabL[i] + f * el;
        }

        /// <summary>Takes the previous stage's plate signal in volts, returns this plate's signal in volts.</summary>
        public double Process(double vin)
        {
            double e = vin - vc, r = rth2;
            double vth = r * (e * kTh + cmT * vg);
            double bk = ak * ckT * vk;

            double target = vth - bk;
            double lo = double.NegativeInfinity, hi = target;
            double xv = Math.Min(x, target);
            double ip, ig;
            int n = 0;
            while (true)
            {
                double l = Lookup(xv, out double g, out double dl, out double dg, out int seg);
                ip = l; ig = g;
                double f = xv + ak * (l + g) + r * g - target;
                n++;
                if (Math.Abs(f) < 1e-9 || n == 32) break;
                if (f > 0) hi = xv; else lo = xv;
                double xn = xv - f / (1 + ak * (dl + dg) + r * dg);
                if (xn > lo && xn < hi)
                {
                    if (Segment(xn) == seg)
                    {
                        ip = l + dl * (xn - xv); ig = g + dg * (xn - xv);
                        xv = xn;
                        break;
                    }
                    xv = xn;
                }
                else
                {
                    if (double.IsNegativeInfinity(lo))
                    {
                        double l0 = Lookup(target, out double g0, out _, out _, out _);
                        lo = target - ak * (l0 + g0) - r * g0;
                    }
                    xv = 0.5 * (lo + hi);
                }
            }
            x = xv;

            double nvk = ak * (ip + ig) + bk;
            double nvg = xv + nvk;
            double iIn = ig + cmT * (nvg - vg);
            double va = nvg + c.Rgs * iIn;
            vc += ccT * (e - va) * invRc;
            vk = nvk; vg = nvg;

            double y = -c.Rp * (ip - ipQ);
            return c.PlateLp > 0 ? lp.LowPass(y) : y;
        }
    }

    /// <summary>The GAIN pot between the first two stages.</summary>
    const double GainPot = 500e3;
    /// <summary>The resistor in series with it.</summary>
    const double GainSeries = 470e3;

    readonly double fs;
    readonly Stage[] stages;
    Biquad bright;
    OnePole dc;
    /// <summary>Smoothed GAIN knob, -1 until first set.</summary>
    double gain = -1;
    bool? lastBright;

    /// <param name="fs">The rate Process is called at (the oversampled rate).</param>
    public HiGain(double fs)
    {
        this.fs = fs;
        Circuit[] circuits =
        {
            new(360, 220e3, 1.8e3, 0.47e-6, 10e3, 0, 0, 1e6, 68e3, 130e-12, 0),
            new(350, 100e3, 1.8e3, 0.47e-6, 49e3, 22e-9, GainSeries, GainPot, 0, 120e-12, 0),
            new(350, 100e3, 39e3, 0, 40e3, 22e-9, 470e3, 1e6, 0, 20e-12, 2000),
            new(360, 220e3, 1.8e3, 0.47e-6, 98e3, 22e-9, 0, 330e3, 220e3, 130e-12, 0),
            new(378, 220e3, 2.2e3, 22e-6, 1e3, 22e-9, 470e3, 15e3, 220e3, 130e-12, 0),
        };
        stages = Array.ConvertAll(circuits, c => new Stage(fs, c));
        dc.SetLowPass(fs, 10);
    }

    /// <summary>Back to the no-signal operating point (called when the channel is selected).</summary>
    public void Reset()
    {
        foreach (var s in stages) s.Reset();
        bright.Reset(); dc.Reset();
        gain = -1;
    }

    /// <summary>Once per block of <paramref name="n"/> base-rate samples: the GAIN knob (0..1) and the BRIGHT switch.</summary>
    public void Set(double gainKnob, bool brightOn, int n, double baseRate)
    {
        if (lastBright != brightOn)
        {
            lastBright = brightOn;
            bright.HighShelf(fs, 2200, brightOn ? 5 : 0);
        }
        double prev = gain;
        if (prev < 0) gain = gainKnob;
        else gain += (1 - Math.Exp(-n / (baseRate * 0.02))) * (gainKnob - prev);
        if (Math.Abs(gain - prev) > 1e-7)
        {
            double w = Math.Max(Math.Pow(Math.Clamp(gain, 0, 1), 2.2), 0.002);
            stages[1].SetInput(GainSeries + (1 - w) * GainPot, w * GainPot);
        }
    }

    /// <summary>One oversampled sample, in volts at the input jack; the result is at the level the
    /// tone stack expects.</summary>
    public double Process(double v)
    {
        v = stages[0].Process(v);
        v = bright.Process(v);
        for (int i = 1; i < stages.Length; i++) v = stages[i].Process(v);
        return dc.HighPass(v) * OutScale;
    }
}
