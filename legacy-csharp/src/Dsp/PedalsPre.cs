using System;

namespace Jwala.Dsp;

// =====================================================================  PRE (before the amp)

/// <summary>
/// Smooth sustainer with parallel blend. The level is read from a held peak of the low-cut
/// sidechain and all the timing is done on the gain reduction in dB, so held notes are not
/// distorted by the gain wobbling within each cycle. Release is program dependent: a pick
/// transient lets go quickly, while a passage that has been held down recovers slowly, so notes
/// bloom without pumping. Highs are reduced a little less than the rest, keeping the sparkle.
/// </summary>
public sealed class Compressor : SampleEffect
{
    /// <summary>Sidechain low cut: keeps the low strings from steering the whole gain.</summary>
    const double ScHz = 110;
    /// <summary>The detector reports the highest peak of the last one to two windows of this length. A window
    /// is longer than half a cycle of the low E, so the level of a held note is flat instead of
    /// rippling at twice the note's pitch.</summary>
    const double HoldS = 0.007;
    /// <summary>Above this the wet path gives back some of the top that gain reduction takes off the pick.</summary>
    const double AirHz = 2200;

    OnePole scHp, air;
    double fs;
    // Highest sidechain peak in the window being filled, and in the one before it.
    double peak, peakPrev;
    int holdLeft, holdLen;
    // Gain reduction in dB: the quick follower, and the slow one that remembers held passages.
    double gFast, gSlow, atk, relFast, slowAtk, slowRel, thr, slope, makeup, blend;
    // Smoothed makeup and blend, so turning a knob does not zipper.
    double sMakeup, sBlend, smooth;
    bool primed;

    public Compressor() : base("comp", "SQUEEZE", EffectSlot.Mono,
        new Param("SUSTAIN", 0.5f), new Param("ATTACK", 0.35f), new Param("LEVEL", 0.5f), new Param("BLEND", 1f, Fmt.Pct))
    { }

    protected override void Init(double f)
    {
        fs = f;
        scHp.SetLowPass(fs, ScHz); air.SetLowPass(fs, AirHz);
        scHp.Reset(); air.Reset();
        peak = peakPrev = 0;
        holdLen = (int)(fs * HoldS); holdLeft = holdLen;
        gFast = gSlow = 0;
        relFast = Coef(fs, 0.07); slowAtk = Coef(fs, 0.25); slowRel = Coef(fs, 0.6); smooth = Coef(fs, 0.02);
        primed = false;
    }

    public override void Update()
    {
        double s = Params[0].Value;
        atk = Coef(fs, Log(Params[1].Value, 0.0008, 0.05));
        thr = -14 - 34 * s;
        double ratio = 2 + 10 * s;
        slope = 1 - 1 / ratio;
        makeup = Db(-thr * slope * 0.55 + (-12 + 24 * Params[2].Value));
        blend = Params[3].Value;
        if (!primed) { primed = true; sMakeup = makeup; sBlend = blend; }
    }

    protected override double Tick(double x)
    {
        // level: held peak of the low-cut sidechain
        double a = Math.Abs(scHp.HighPass(x));
        if (holdLeft == 0) { holdLeft = holdLen; peakPrev = peak; peak = 0; }
        holdLeft--;
        peak = Math.Max(peak, a);
        double over = 20 * Math.Log10(Math.Max(peak, peakPrev) + 1e-9) - thr;
        // soft knee (12 dB)
        double target = over <= -6 ? 0 : over >= 6 ? over * slope : slope * (over + 6) * (over + 6) / 24;
        // timing, in dB
        gFast += (target > gFast ? atk : relFast) * (target - gFast);
        gSlow += (gFast > gSlow ? slowAtk : slowRel) * (gFast - gSlow);
        double gr = Math.Max(gFast, 0.7 * gSlow);
        double g = Db(-gr);
        // the top is turned down a little less, by up to 4 dB
        double hi = air.HighPass(x);
        sMakeup += smooth * (makeup - sMakeup);
        sBlend += smooth * (blend - sBlend);
        double y = g * (x + Math.Min(0.03 * gr, 0.6) * hi) * sMakeup;
        return x + (y - x) * sBlend;
    }
}

/// <summary>Diode constants shared by the drive and fuzz models.</summary>
static class Diode
{
    /// <summary>Diode saturation current and slope voltage (1N914).</summary>
    public const double Is = 2.52e-9, Nvt = 1.752 * 25.85e-3;
}

/// <summary>
/// Green screamer: mid-hump soft clipping overdrive, modelled on the circuit. The clipping stage
/// is an op-amp with the diodes and the DRIVE pot in its feedback loop, solved every sample for
/// the voltage that carries the input current, with the clean signal riding on top: quiet
/// playing passes almost clean and the knee rounds over gradually as you dig in. The small cap
/// across the loop closes the top end down as DRIVE comes up. The solved curve is anti-aliased
/// the same way as SoftClip (averaged over the step from the previous sample). TONE is the
/// pedal's own active control, from the bare 723 Hz low-pass up to its full upper-mid lift.
/// </summary>
public sealed class Overdrive : SampleEffect
{
    // Clipping stage: an op-amp with the diodes, a small cap and the DRIVE pot in its
    // feedback loop, and a resistor + cap leg to ground that sets where the gain starts (720 Hz).
    const double LegR = 4.7e3, LegC = 47e-9, FbC = 51e-12;
    const double Is = Diode.Is, Nvt = Diode.Nvt;
    // The diode facing the other way is a softer one: it starts conducting earlier and gives way
    // more gradually, so the two halves of the wave are rounded differently and some second
    // harmonic stays in the sound.
    const double IsB = 12 * Is, NvtB = 1.5 * Nvt;
    // Tone stage: 1k + 0.22 uF low-pass (723 Hz), then an op-amp whose 20k pot steers a 220 ohm +
    // 0.22 uF leg between cutting at its input and boosting in its feedback (1k).
    const double ToneR = 1e3, ToneC = 0.22e-6, TonePot = 20e3, ToneRs = 220;

    OnePole inp, leg, cap, outHp;
    Biquad tone;
    double fs;
    /// <summary>DRIVE resistance as a conductance.</summary>
    double gr;
    // Last sample's feedback current, the voltage across the diodes that carries it, and the
    // integral of that voltage over current up to there.
    double i1, v1, f1, lvl;

    public Overdrive() : base("od", "GREEN DRIVE", EffectSlot.Oversampled,
        new Param("DRIVE", 0.35f), new Param("TONE", 0.5f), new Param("LEVEL", 0.6f))
    { }

    /// <summary>What the op-amp can swing: clean up to the knee, then rounding into the supply rail. Only a
    /// hot boost in front of the pedal gets this far.</summary>
    static double Rail(double u)
    {
        const double Knee = 3.0, Span = 0.8;
        double a = Math.Abs(u);
        return a <= Knee ? u : Math.CopySign(Knee + Span * Math.Tanh((a - Knee) / Span), u);
    }

    protected override void Init(double f)
    {
        fs = f;
        inp.SetLowPass(fs, 20);
        leg.SetLowPass(fs, 1 / (2 * Math.PI * LegR * LegC));
        outHp.SetLowPass(fs, 16);
        inp.Reset(); leg.Reset(); cap.Reset(); outHp.Reset(); tone.Reset();
        i1 = v1 = f1 = 0;
    }

    public override void Update()
    {
        double d = Params[0].Value;
        gr = 1 / (51e3 + d * 500e3);
        cap.SetLowPass(fs, gr / (2 * Math.PI * FbC));
        // TONE: the leg's series resistance (pot halves in parallel + 220 ohm) sweeps from the
        // pot's centre, where the leg does nothing, to the full-boost end
        double r = Log(Math.Pow(Params[1].Value, 0.75), ToneRs + TonePot / 4, ToneRs);
        double k = 0.5 + 0.5 * Math.Sqrt(Math.Max(1 - 4 * (r - ToneRs) / TonePot, 0));
        double rc = ToneR * ToneC, c = ToneC;
        tone.Analog(fs, 1, c * (r + k * ToneR), 0, 1, rc + c * r + ToneR * (1 - k) * c, rc * c * r);
        lvl = Db(-16 + 26 * Params[2].Value);
    }

    protected override double Tick(double x)
    {
        x = inp.HighPass(x);
        // current pulled through the leg to ground; all of it has to come round the feedback loop
        double i = Math.Clamp(cap.LowPass(leg.HighPass(x) / LegR), -0.02, 0.02);
        // Newton for the voltage at which DRIVE resistor + diodes carry that current
        double v = v1, ep = 0, en = 0;
        for (int it = 0; it < 24; it++)
        {
            ep = Is * Math.Exp(v / Nvt); en = IsB * Math.Exp(-v / NvtB);
            double fv = gr * v + (ep - Is) - (en - IsB) - i;
            double dv = Math.Clamp(fv / (gr + ep / Nvt + en / NvtB), -0.1, 0.1);
            v -= dv;
            if (Math.Abs(dv) < 1e-10) break;
        }
        // integral of v over i, in closed form from the solution: v i - (integral of i over v)
        double f = v * i - (0.5 * gr * v * v + Nvt * (ep - Is) - Is * v + NvtB * (en - IsB) + IsB * v);
        double di = i - i1;
        double vd = Math.Abs(di) < 1e-10 ? 0.5 * (v + v1) : (f - f1) / di;
        i1 = i; v1 = v; f1 = f;
        double y = outHp.HighPass(Rail(x + vd));
        return tone.Process(y) * lvl;
    }
}

/// <summary>
/// Soft clipper with first-order antiderivative anti-aliasing: the output is the clipping curve
/// averaged over the step from the previous sample to this one, which takes most of the aliasing
/// out of hard-driven edges. The curve is tanh on top and a lower, softer tanh underneath
/// (floor is how far the bottom half can swing; 1.0 makes it symmetric).
/// </summary>
struct SoftClip
{
    double x1, f1;

    /// <summary>ln(cosh(u)), the antiderivative of tanh, written so it cannot overflow.</summary>
    public static double LnCosh(double u)
    {
        double a = Math.Abs(u);
        return a + Math.Log(1 + Math.Exp(-2 * a)) - Math.Log(2);
    }

    static double Curve(double u, double floor) => u >= 0 ? Math.Tanh(u) : floor * Math.Tanh(u / floor);
    static double Integral(double u, double floor) => u >= 0 ? LnCosh(u) : floor * floor * LnCosh(u / floor);

    /// <summary>Starts at rest with <paramref name="x"/> at the input, so a biased stage does not open with a blip.</summary>
    public void Reset(double x, double floor) { x1 = x; f1 = Integral(x, floor); }

    public double Tick(double x, double floor)
    {
        double f = Integral(x, floor), dx = x - x1;
        double y = Math.Abs(dx) < 1e-5 ? Curve(0.5 * (x + x1), floor) : (f - f1) / dx;
        x1 = x; f1 = f;
        return y;
    }
}

/// <summary>
/// Two cascaded soft-clipping stages, valve fashion, with a sweepable filter. The lows are thinned
/// before the first stage (more so as DIST rises) so chords stay clear, and come back through a
/// gently driven parallel band so the sound stays thick. The first stage's bias follows the playing
/// level, which shifts the duty cycle and keeps second harmonic in the sound however hard it is
/// driven.
/// </summary>
public sealed class Distortion : SampleEffect
{
    /// <summary>Resting bias on the first stage.</summary>
    const double Bias = 0.2;
    OnePole hp, low, pre, mid, couple, dc, filt, filt2;
    SoftClip c1, c2;
    double fs, env, atk, rel, g1, g2, tight, gLow, lvl;

    public Distortion() : base("dist", "RODENT", EffectSlot.Oversampled,
        new Param("DIST", 0.5f), new Param("FILTER", 0.45f), new Param("VOLUME", 0.5f))
    { }

    protected override void Init(double f)
    {
        fs = f;
        hp.SetLowPass(fs, 60); low.SetLowPass(fs, 220); pre.SetLowPass(fs, 9000);
        mid.SetLowPass(fs, 5500); couple.SetLowPass(fs, 90); dc.SetLowPass(fs, 20);
        hp.Reset(); low.Reset(); pre.Reset(); mid.Reset(); couple.Reset(); dc.Reset(); filt.Reset(); filt2.Reset();
        c1.Reset(Bias, 0.7); c2.Reset(0, 0.85);
        env = 0; atk = Coef(fs, 0.003); rel = Coef(fs, 0.08);
    }

    public override void Update()
    {
        double d = Params[0].Value;
        g1 = Log(d, 3, 250);
        g2 = 2 + 4 * d;
        tight = 0.4 + 0.45 * d;
        gLow = 2 + 0.1 * g1;
        double fc = Log(1 - Params[1].Value, 500, 16000);
        filt.SetLowPass(fs, fc); filt2.SetLowPass(fs, fc * 1.6);
        lvl = Db(-26 + 26 * Params[2].Value);
    }

    protected override double Tick(double x)
    {
        double h = hp.HighPass(x);
        double lo = low.LowPass(h);
        double p = pre.LowPass(h - tight * lo) * g1;
        double a = Math.Abs(p);
        env += (a > env ? atk : rel) * (a - env);
        // first stage: uneven halves, bias riding on the envelope
        double s1 = c1.Tick(p + Bias + 0.3 * env, 0.7) - Math.Tanh(Bias);
        double c = couple.HighPass(mid.LowPass(s1));
        // second stage, leaning the other way
        double s2 = dc.HighPass(c2.Tick(-c * g2, 0.85));
        double v = -0.44 * s2 + 0.22 * Math.Tanh(lo * gLow);
        return filt2.LowPass(filt.LowPass(v)) * lvl;
    }
}

/// <summary>
/// Big wall-of-fuzz, modelled on the circuit: a booster, the SUSTAIN pot, two clipping stages
/// and the scooped tone stack. Each clipping stage is a gain stage with a diode pair in its
/// feedback loop, solved every sample for the voltage that carries the input current, so the
/// knee is the diodes' own gradual one and notes sink into the fuzz smoothly as they die away.
/// The feedback cap makes each stage dark until the diodes conduct, which is what keeps the
/// sustain thick and singing instead of fizzy; the coupling caps between the stages keep chords
/// from turning to mud. The second stage leans off centre in proportion to the level it is fed,
/// which shifts the duty cycle of the wave and keeps second harmonic in the sound from the pick
/// to the tail of the note. TONE is the pedal's passive stack: lows only at one end, highs only at
/// the other, the mids scooped in between.
/// </summary>
public sealed class Fuzz : SampleEffect
{
    // Clipping stage: a transistor gain stage fed through a coupling cap and a resistor, with
    // a 470k resistor, a small cap and a pair of diodes from its output back to its input.
    const double InR = 10e3, InC = 0.1e-6, FbR = 470e3, FbC = 470e-12;
    const double Is = Diode.Is, Nvt = Diode.Nvt;
    // The two diodes of a pair are never quite alike: the second conducts a little earlier and
    // gives way a little more gradually, so the halves of the wave differ and the fuzz keeps some
    // even harmonics.
    const double IsB = 2.5 * Is, NvtB = 1.12 * Nvt;
    /// <summary>Gain of the booster in front of the SUSTAIN pot.</summary>
    const double Boost = 6.0;
    /// <summary>Once the diodes conduct, the feedback cap only rounds the edges of the wave.</summary>
    const double EdgeHz = 7000;
    /// <summary>How far the second stage is pushed off centre, as a fraction of the level it is being fed.</summary>
    const double Lean = 0.05;
    // Tone stack: a 39k + 10 nF low-pass and a 4 nF + 22k high-pass, with the 100k TONE pot strung
    // between their outputs.
    const double ToneR1 = 39e3, ToneC1 = 10e-9, ToneC2 = 4e-9, ToneR2 = 22e3, TonePot = 100e3;
    /// <summary>Recovery gain after the tone stack: with VOLUME at noon the fuzz sits level with the drives.</summary>
    const double Makeup = 6.4;

    /// <summary>One clipping stage: returns the voltage across the feedback loop (resistor and
    /// diode pair) that carries the input current, anti-aliased by averaging the solved curve
    /// over the step from the previous sample, as Overdrive does.</summary>
    struct Clip
    {
        double i1, v1, f1;
        public void Reset() => i1 = v1 = f1 = 0;
        public double Tick(double i)
        {
            const double Gr = 1 / FbR;
            i = Math.Clamp(i, -5e-3, 5e-3);
            double v = v1, ep = 0, en = 0;
            for (int it = 0; it < 24; it++)
            {
                ep = Is * Math.Exp(v / Nvt); en = IsB * Math.Exp(-v / NvtB);
                double fv = Gr * v + (ep - Is) - (en - IsB) - i;
                double dv = Math.Clamp(fv / (Gr + ep / Nvt + en / NvtB), -0.1, 0.1);
                v -= dv;
                if (Math.Abs(dv) < 1e-10) break;
            }
            // integral of v over i, in closed form from the solution: v i - (integral of i over v)
            double f = v * i - (0.5 * Gr * v * v + Nvt * (ep - Is) - Is * v + NvtB * (en - IsB) + IsB * v);
            double di = i - i1;
            double vd = Math.Abs(di) < 1e-11 ? 0.5 * (v + v1) : (f - f1) / di;
            i1 = i; v1 = v; f1 = f;
            return vd;
        }
    }

    OnePole inp, boost, hp1, hp2, cap1, cap2, edge1, edge2, outHp;
    Clip c1, c2;
    Biquad tone;
    double fs, env, atk, rel, sustain, lvl;

    public Fuzz() : base("fuzz", "MUFFLER", EffectSlot.Oversampled,
        new Param("SUSTAIN", 0.6f), new Param("TONE", 0.5f), new Param("VOLUME", 0.5f))
    { }

    protected override void Init(double f)
    {
        fs = f;
        inp.SetLowPass(fs, 30); boost.SetLowPass(fs, 3400);
        double couple = 1 / (2 * Math.PI * InR * InC);
        hp1.SetLowPass(fs, couple); hp2.SetLowPass(fs, couple);
        double capHz = 1 / (2 * Math.PI * FbR * FbC);
        cap1.SetLowPass(fs, capHz); cap2.SetLowPass(fs, capHz);
        edge1.SetLowPass(fs, EdgeHz); edge2.SetLowPass(fs, EdgeHz);
        outHp.SetLowPass(fs, 20);
        inp.Reset(); boost.Reset(); hp1.Reset(); hp2.Reset(); cap1.Reset(); cap2.Reset(); edge1.Reset(); edge2.Reset(); outHp.Reset();
        c1.Reset(); c2.Reset(); tone.Reset();
        env = 0; atk = Coef(fs, 0.003); rel = Coef(fs, 0.08);
    }

    public override void Update()
    {
        sustain = Boost * Log(Params[0].Value, 0.01, 1.0);
        // TONE: the output is taken from the pot's wiper, t of the way from the low-pass end to
        // the high-pass end
        double t = Params[1].Value;
        double g1 = 1 / ToneR1, g2 = 1 / ToneR2, gp = 1 / TonePot;
        double c1v = ToneC1, c2v = ToneC2;
        tone.Analog(fs,
            g1 * (g2 + gp) - t * g1 * g2, c2v * (g1 + gp), t * c1v * c2v,
            (g1 + gp) * (g2 + gp) - gp * gp, c1v * (g2 + gp) + c2v * (g1 + gp), c1v * c2v);
        lvl = Db(-26 + 24 * Params[2].Value) * Makeup;
    }

    protected override double Tick(double x)
    {
        x = boost.LowPass(inp.HighPass(x)) * sustain;
        // each stage inverts
        double i = cap1.LowPass(hp1.HighPass(x) / InR);
        double s1 = -edge1.LowPass(c1.Tick(i));
        i = cap2.LowPass(hp2.HighPass(s1) / InR);
        double a = Math.Abs(i);
        env += (a > env ? atk : rel) * (a - env);
        // the second pair is in the other way round
        double s2 = edge2.LowPass(c2.Tick(-i - Lean * env));
        return tone.Process(outHp.HighPass(s2)) * lvl;
    }
}
