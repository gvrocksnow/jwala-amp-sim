using System;

namespace Jwala.Dsp;

/// <summary>
/// The LEAD II power amp: phase inverter, a cold-biased push-pull pair, output transformer and
/// speaker load, wrapped in a global negative-feedback loop that holds the PRESENCE and DEPTH
/// controls.
///
/// Power valves are current sources, so without feedback the voltage on the speaker follows
/// its impedance: a bump at the cabinet resonance and a rise towards the treble. Feedback
/// flattens that. PRESENCE and DEPTH work by taking feedback away above and below the mids,
/// which lets the speaker's own shape come back there. Once the valves clip there is no loop
/// gain left, so the feedback (and with it both controls) lets go: the amp gets looser and
/// brighter exactly when it is pushed. A plain EQ in front of a clipper cannot do that.
///
/// The loop has no delay in it. Per sample the forward path is a rising function of the
/// error voltage at the phase inverter, and every filter's output is its input times a
/// constant plus a term from its state, so the loop closes by solving one scalar equation.
///
/// Levels: input and output are normalised so that 1.0 is the clipping point, as for the
/// other channels' power stage. Inside, grid voltages are in volts.
/// </summary>
public sealed class PowerAmp
{
    // Phase inverter: volts of drive per unit of error, and the most it can swing per side.
    const double PiGain = 190, PiSwing = 70;
    // One side of the phase inverter has a little less gain than the other, as in a real
    // long-tailed pair; the mismatch is what leaves some even harmonics after the push-pull.
    const double PiBalance = 0.95;
    // Power valve bias and cutoff (volts below the cathode). The 8 V between them is a cold
    // bias: about 5% of peak current at idle, so there is a crossover region.
    const double Bias = 52, Cutoff = 60;
    // Speaker load: mid-band volts per unit of valve current, the height of the resonance bump
    // and of the inductive rise above it.
    const double Load = 1.15, SpkResHz = 100, SpkResQ = 1.5, SpkRes = 1.5, SpkRiseHz = 4000, SpkRise = 2;
    // Fraction of the output fed back to the phase inverter.
    const double Feedback = 0.75;
    // The most of that feedback PRESENCE and DEPTH can take away, and where they act.
    const double ToneRange = 0.8, PresenceHz = 1200, DepthHz = 250;
    // Supply droop at full continuous current.
    const double Sag = 0.10;
    // Output level: this stage clips squarer than the other channels' power stage, so at the same
    // peak it is louder; this brings it back to their loudness.
    const double OutTrim = 0.78;
    // Range and size of the valve table: drive at one grid, in volts either side of zero.
    const double TabU = 160;
    const int TabN = 4096;
    const double TabStep = 2 * TabU / TabN;
    // Bound on the forward path's output, for bracketing the solve.
    const double OutMax = 1.05;

    /// <summary>Smooth max(a, 0) with a knee <paramref name="w"/> wide.</summary>
    static double Softplus(double a, double w)
    {
        double t = a / w;
        if (t > 30) return a;
        if (t < -30) return 0;
        return w * Math.Log(1 + Math.Exp(t));
    }

    /// <summary>(soft clip to +-1, its slope), gentle knee: the phase inverter.</summary>
    static (double, double) ClipSoft(double x)
    {
        double r = 1 / Math.Sqrt(1 + x * x);
        return (x * r, r * r * r);
    }

    /// <summary>(soft clip to +-1, its slope), sharp knee: the valves running out of plate voltage.</summary>
    static (double, double) ClipHard(double x)
    {
        double x2 = x * x, iq = 1 / (1 + x2 * x2), r = Math.Sqrt(Math.Sqrt(iq));
        return (x * r, r * iq);
    }

    /// <summary>Trapezoidal one-pole low-pass whose output is available as gain * input + state term
    /// before the input is known.</summary>
    struct Pole
    {
        public double A, B, S; // g / (1 + g) and 1 / (1 + g) for the prewarped g
        public void Set(double fs, double hz) { double g = Math.Tan(Math.PI * hz / fs); A = g / (1 + g); B = 1 / (1 + g); }
        public readonly (double, double) Peek() => (A, S * B);
        public void Push(double x) { double v = (x - S) * A; S += 2 * v; }
    }

    /// <summary>Trapezoidal state-variable band-pass (unity gain at the centre), same idea.</summary>
    struct Band
    {
        public double K, A1, A2, A3, Ic1, Ic2;
        public void Set(double fs, double hz, double q)
        {
            double g = Math.Tan(Math.PI * hz / fs);
            K = 1 / q; A1 = 1 / (1 + g * (g + K)); A2 = g * A1; A3 = g * A2;
        }
        public readonly (double, double) Peek() => (K * A2, K * (A1 * Ic1 - A2 * Ic2));
        public void Push(double x)
        {
            double v3 = x - Ic2, v1 = A1 * Ic1 + A2 * v3, v2 = Ic2 + A2 * Ic1 + A3 * v3;
            Ic1 = 2 * v1 - Ic1; Ic2 = 2 * v2 - Ic2;
        }
    }

    /// <summary>One power valve against the drive <paramref name="u"/> arriving at its grid: plate current
    /// as a fraction of its peak, and volts by which the grid was driven past the point where it conducts.</summary>
    static (double I, double Over) Valve(double u)
    {
        // the grid cannot go above the cathode: past that it conducts and clamps
        double over = Softplus(u - Bias, 1.5);
        // three-halves power law above cutoff
        double s = Softplus(u - over - Bias + Cutoff, 2);
        return (Math.Pow(s / Cutoff, 1.5), over);
    }

    readonly double fs;
    Band spkRes;
    Pole spkRise, pres, dep;
    double kp, kd;
    // Charge on the two coupling caps into the power valve grids: grid current pushes the
    // bias colder after the valves have been driven hard, and it leaks back over a few ms.
    double vc0, vc1;
    readonly double capCharge, capLeak;
    // Supply droop, the same seen more slowly by the preamp's rail, and its smoothing.
    double droop, droopSlow;
    readonly double droopK, droopSlowK;
    // Output transformer core flux and the share of the output it takes when pushed further.
    double flux, loss;
    readonly double fluxLeak, fluxSat;
    OnePole ot;
    /// <summary>Current the pair draws at rest.</summary>
    readonly double idle;
    /// <summary>Valve tabulated over -TabU..TabU, so the solve needs no exponentials.</summary>
    readonly double[] tabI = new double[TabN + 1], tabO = new double[TabN + 1];
    /// <summary>The last two solutions.</summary>
    double e, e1;

    // the forward path's results for the last error voltage tried
    double fy, fdy, fOver0, fOver1, fI0, fI1;

    /// <param name="fs">The rate Process is called at (the oversampled rate).</param>
    public PowerAmp(double fs)
    {
        this.fs = fs;
        double t = 1 / fs;
        spkRes.Set(fs, SpkResHz, SpkResQ);
        spkRise.Set(fs, SpkRiseHz);
        pres.Set(fs, PresenceHz);
        dep.Set(fs, DepthHz);
        ot.SetLowPass(fs, 18000);
        // 22 nF fed from ~91k charges in 2 ms and leaks through the 220k grid resistor in 4.8 ms
        capCharge = t / 0.002;
        capLeak = t / 0.0048;
        droopK = 1 - Math.Exp(-t / 0.025);
        droopSlowK = 1 - Math.Exp(-t / 0.15);
        fluxLeak = t * 2 * Math.PI * 8;
        // the core starts to saturate around a full-level 45 Hz sine
        fluxSat = 1 / (2 * Math.PI * 45);
        idle = 2 * Valve(0).I;
        for (int i = 0; i <= TabN; i++) (tabI[i], tabO[i]) = Valve(-TabU + i * TabStep);
    }

    public void Reset()
    {
        spkRes.Ic1 = spkRes.Ic2 = 0;
        spkRise.S = pres.S = dep.S = 0;
        vc0 = vc1 = 0;
        droop = droopSlow = 0;
        flux = loss = 0;
        ot.Reset();
        e = e1 = 0;
    }

    /// <summary>Valve current (returned), grid overdrive and the current's slope for drive <paramref name="u"/> at a grid.
    /// Below the table the valve is cut off; above it the grid is clamped, which the last segment's slopes continue.</summary>
    double Lookup(double u, out double over, out double di)
    {
        double p = (u + TabU) * (1 / TabStep);
        if (p <= 0) { over = 0; di = 0; return 0; }
        int i = (int)Math.Min((long)p, TabN - 1);
        double f = p - i;
        double dI = tabI[i + 1] - tabI[i], dO = tabO[i + 1] - tabO[i];
        over = tabO[i] + f * dO;
        di = dI * (1 / TabStep);
        return tabI[i] + f * dI;
    }

    /// <summary>PRESENCE and DEPTH, 0..1.</summary>
    public void Set(double presence, double depth)
    {
        kp = ToneRange * Math.Clamp(presence, 0, 1);
        kd = ToneRange * Math.Clamp(depth, 0, 1);
    }

    /// <summary>What the supply droop leaves of the preamp's rail (1.0 at rest).</summary>
    public double Rail => 1 - 0.5 * droopSlow;

    /// <summary>Phase inverter -> grids -> valves -> speaker load -> supply limit, for error voltage <paramref name="ev"/>.
    /// <paramref name="z0"/> and <paramref name="sz"/> are the speaker load's gain and state term for this sample;
    /// <paramref name="headroom"/> is what the sagging supply leaves, <paramref name="screen"/> what that leaves of the valves' current.</summary>
    void Forward(double ev, double z0, double sz, double headroom, double invHeadroom, double screen)
    {
        var (d, dd) = ClipSoft(ev * (PiGain / PiSwing));
        d *= PiSwing; dd *= PiGain;
        // drive at each grid, less what has collected on its coupling cap
        double i0 = Lookup(d - vc0, out fOver0, out double di0);
        double i1 = Lookup(-PiBalance * d - vc1, out fOver1, out double di1);
        fI0 = screen * i0; fI1 = screen * i1;
        double di = screen * di0 * dd + PiBalance * screen * di1 * dd;
        double v = z0 * (fI0 - fI1) + sz;
        var (c, dc) = ClipHard(v * invHeadroom);
        // a core near saturation only resists being pushed further the same way
        double keep = c * flux > 0 ? 1 - loss : 1;
        fy = headroom * c * keep;
        fdy = dc * z0 * di * keep;
    }

    /// <summary>One oversampled sample.</summary>
    public double Process(double x)
    {
        // the filters around the loop, each as gain * input + state term
        var (gb, sb) = spkRes.Peek();
        var (gh, sh) = spkRise.Peek();
        double z0 = Load * (1 + SpkRes * gb + SpkRise * (1 - gh));
        double sz = Load * (SpkRes * sb - SpkRise * sh);
        var (gp, sp) = pres.Peek();
        var (gd, sd) = dep.Peek();
        double g0 = Feedback * (1 - kp * (1 - gp) - kd * gd);
        double sg = Feedback * (kp * sp - kd * sd);

        // error = input - feedback: e + g0 * y(e) = x - sg. The left side rises with e, so
        // Newton is kept inside a bracket and falls back to bisection.
        // screens sag with the supply, and the valves pass less current for the same grid voltage
        double headroom = 1 - droop, invHeadroom = 1 / headroom, screen = headroom * Math.Sqrt(headroom);
        double target = x - sg;
        double lo = target - g0 * OutMax, hi = target + g0 * OutMax;
        // start from where the last two solutions were heading
        double ev = Math.Clamp(2 * e - e1, lo, hi);
        e1 = e;
        Forward(ev, z0, sz, headroom, invHeadroom, screen);
        for (int it = 0; it < 40; it++)
        {
            double r = ev + g0 * fy - target;
            if (Math.Abs(r) < 1e-6) break;
            if (r > 0) hi = ev; else lo = ev;
            double en = ev - r / (1 + g0 * fdy);
            ev = en > lo && en < hi ? en : 0.5 * (lo + hi);
            Forward(ev, z0, sz, headroom, invHeadroom, screen);
        }
        e = ev;
        double y = fy;

        // advance the state
        double id = fI0 - fI1;
        spkRes.Push(id);
        spkRise.Push(id);
        pres.Push(y);
        dep.Push(y);
        vc0 += capCharge * fOver0 - capLeak * vc0;
        vc1 += capCharge * fOver1 - capLeak * vc1;
        // the supply sags with the current drawn beyond idle
        double draw = Sag * Math.Max(fI0 + fI1 - idle, 0);
        droop += droopK * (draw - droop);
        droopSlow += droopSlowK * (droop - droopSlow);
        // the transformer core integrates the output; near saturation it takes some of it back
        flux += y / fs - fluxLeak * flux;
        double fl = Math.Abs(flux / fluxSat);
        loss = Math.Min(0.2 * fl * fl * fl, 0.6);

        return ot.LowPass(y) * OutTrim;
    }
}
