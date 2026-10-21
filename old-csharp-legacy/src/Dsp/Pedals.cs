using System;

namespace Jwala.Dsp;

static class Fmt
{
    public static string Ms(double ms) => ms >= 1000 ? (ms / 1000).ToString("0.00") + " s" : ms.ToString("0") + " ms";
    public static string Pct(float v) => (v * 100).ToString("0") + "%";
    public static string Hz(double hz) => hz >= 1000 ? (hz / 1000).ToString("0.0") + " kHz" : hz.ToString("0") + " Hz";
}

// =====================================================================  PRE (before the amp)

/// <summary>Studio-style optical-ish compressor with parallel blend.</summary>
public sealed class Compressor : SampleEffect
{
    double env, gain = 1, atk, rel, fs;
    public Compressor() : base("comp", "SQUEEZE", EffectSlot.Mono,
        new Param("SUSTAIN", 0.5f), new Param("ATTACK", 0.35f), new Param("LEVEL", 0.5f), new Param("BLEND", 1f, Fmt.Pct))
    { }

    protected override void Init(double f) { fs = f; env = 0; gain = 1; rel = Coef(fs, 0.25); }

    double thr, slope, makeup, blend;
    public override void Update()
    {
        double s = Params[0].Value;
        atk = Coef(fs, Log(Params[1].Value, 0.0008, 0.05));
        thr = -14 - 34 * s;
        double ratio = 2 + 10 * s;
        slope = 1 - 1 / ratio;
        makeup = Db(-thr * slope * 0.55 + (-12 + 24 * Params[2].Value));
        blend = Params[3].Value;
    }

    protected override double Tick(double x)
    {
        double a = Math.Abs(x);
        env += (a > env ? atk : rel) * (a - env);
        double lvl = 20 * Math.Log10(env + 1e-9);
        double over = lvl - thr;
        // soft knee (6 dB)
        double gr = over <= -3 ? 0 : over >= 3 ? over * slope : slope * (over + 3) * (over + 3) / 12;
        double tg = Db(-gr);
        gain += 0.02 * (tg - gain);
        double y = x * gain * makeup;
        return x * (1 - blend) + y * blend;
    }
}

/// <summary>Green screamer: mid-hump soft clipping overdrive.</summary>
public sealed class Overdrive : SampleEffect
{
    OnePole hp, clipLp, tone; double fs, g, lvl;
    public Overdrive() : base("od", "GREEN DRIVE", EffectSlot.Oversampled,
        new Param("DRIVE", 0.35f), new Param("TONE", 0.5f), new Param("LEVEL", 0.6f))
    { }

    protected override void Init(double f) { fs = f; hp.SetLowPass(fs, 720); clipLp.SetLowPass(fs, 7500); hp.Reset(); clipLp.Reset(); tone.Reset(); }

    public override void Update()
    {
        g = (51e3 + Params[0].Value * 500e3) / 4.7e3;
        tone.SetLowPass(fs, Log(Params[1].Value, 650, 7000));
        lvl = Db(-18 + 26 * Params[2].Value);
    }

    protected override double Tick(double x)
    {
        double c = clipLp.LowPass(hp.HighPass(x) * g);
        c = c > 0 ? 0.62 * Math.Tanh(c / 0.62) : 0.55 * Math.Tanh(c / 0.55); // mismatched diodes
        return tone.LowPass(x + c) * lvl;
    }
}

/// <summary>Op-amp hard clipper (rodent style) with sweepable filter.</summary>
public sealed class Distortion : SampleEffect
{
    OnePole hp, gbw, filt, filt2; double fs, g, lvl;
    public Distortion() : base("dist", "RODENT", EffectSlot.Oversampled,
        new Param("DIST", 0.5f), new Param("FILTER", 0.45f), new Param("VOLUME", 0.5f))
    { }

    protected override void Init(double f) { fs = f; hp.SetLowPass(fs, 70); hp.Reset(); gbw.Reset(); filt.Reset(); filt2.Reset(); }

    public override void Update()
    {
        g = Log(Params[0].Value, 2, 2200);
        gbw.SetLowPass(fs, Math.Clamp(4e6 / g, 2500, 20000)); // op-amp runs out of bandwidth at high gain
        double fc = Log(1 - Params[1].Value, 500, 16000);
        filt.SetLowPass(fs, fc); filt2.SetLowPass(fs, fc * 1.6);
        lvl = Db(-26 + 26 * Params[2].Value);
    }

    protected override double Tick(double x)
    {
        double v = gbw.LowPass(hp.HighPass(x) * g);
        v = 0.65 * v / Math.Pow(1 + Math.Pow(Math.Abs(v / 0.65), 2.5), 0.4); // silicon diodes to ground
        return filt2.LowPass(filt.LowPass(v)) * lvl;
    }
}

/// <summary>Big wall-of-fuzz: two cascaded clipping stages and a mid-scoop tone control.</summary>
public sealed class Fuzz : SampleEffect
{
    OnePole hp1, hp2, lp1, lp2, toneLp, toneHp; double fs, g1, lvl, tone;
    public Fuzz() : base("fuzz", "MUFFLER", EffectSlot.Oversampled,
        new Param("SUSTAIN", 0.6f), new Param("TONE", 0.5f), new Param("VOLUME", 0.5f))
    { }

    protected override void Init(double f)
    {
        fs = f; hp1.SetLowPass(fs, 90); hp2.SetLowPass(fs, 120); lp1.SetLowPass(fs, 6000); lp2.SetLowPass(fs, 5000);
        toneLp.SetLowPass(fs, 900); toneHp.SetLowPass(fs, 1100);
    }

    public override void Update()
    {
        g1 = Log(Params[0].Value, 6, 400);
        tone = Params[1].Value;
        lvl = Db(-26 + 24 * Params[2].Value);
    }

    static double Clip(double v) => v / Math.Pow(1 + Math.Pow(Math.Abs(v), 2.5), 0.4);

    protected override double Tick(double x)
    {
        double s = lp1.LowPass(Clip(hp1.HighPass(x) * g1));
        s = lp2.LowPass(Clip(hp2.HighPass(s) * 18));
        double y = toneLp.LowPass(s) * (1 - tone) + toneHp.HighPass(s) * tone;
        return y * lvl * 1.8;
    }
}

// =====================================================================  POST (after the cab, stereo)

/// <summary>
/// Stereo doubler: fakes a double-tracked part. Two synthetic "takes" of the signal get their
/// own slowly wandering timing (and therefore pitch), level and tone, then are spread left/right.
/// </summary>
public sealed class Doubler : StereoEffect
{
    /// <summary>One synthetic performance: smoothed random drift of timing and level.</summary>
    sealed class Take
    {
        public double Cur, Mid, Target, Gain = 1, GainTarget = 1, Timer;
        public OnePole Tone;
        readonly Random rng;
        public Take(int seed) => rng = new Random(seed);
        public void Step(double fs, double slew)
        {
            if (--Timer <= 0)
            {
                // a new "intention" every 120-380 ms, like a player drifting around the beat
                Target = rng.NextDouble() * 2 - 1;
                GainTarget = 1 + (rng.NextDouble() * 2 - 1) * 0.07;
                Timer = fs * (0.12 + 0.26 * rng.NextDouble());
            }
            Mid += slew * (Target - Mid);   // two cascaded smoothers: drift rate (= pitch) changes glide, never step
            Cur += slew * (Mid - Cur);
            Gain += slew * (GainTarget - Gain);
        }
    }

    DelayLine dl;
    readonly Take a = new(11), b = new(23);
    OnePole hpB;
    double fs, fade, fadeStep;
    public static double DelayMs(float v) => Log(v, 6, 45);

    public Doubler() : base("doubler", "DOUBLER",
        new Param("AMOUNT", 0.5f, Fmt.Pct), new Param("DELAY", 0.35f, v => DelayMs(v).ToString("0") + " ms"),
        new Param("WIDTH", 1f, Fmt.Pct), new Param("MIX", 0.85f, Fmt.Pct))
    { }

    public override void Prepare(double f)
    {
        fs = f; dl = new DelayLine((int)(fs * 0.06));
        a.Tone.SetLowPass(fs, 12000); b.Tone.SetLowPass(fs, 8500); hpB.SetLowPass(fs, 90);
        fadeStep = 1 / (fs * 0.02); fade = Enabled ? 1 : 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        if (!Enabled && fade == 0) return;
        double amt = Params[0].Value, baseB = DelayMs(Params[1].Value) * fs / 1000, w = Params[2].Value, mix = Params[3].Value;
        double baseA = 0.0015 * fs;                       // take 1 sits nearly on the original
        double varA = amt * 0.0006 * fs, varB = amt * 0.0016 * fs; // AMOUNT 50% ~ ±5 cents of drift
        double slew = Coef(fs, 0.22);
        for (int i = 0; i < l.Length; i++)
        {
            double t = Enabled ? 1 : 0;
            fade = fade < t ? Math.Min(t, fade + fadeStep) : Math.Max(t, fade - fadeStep);
            double mono = 0.5 * (l[i] + r[i]);
            dl.Write(mono);
            a.Step(fs, slew); b.Step(fs, slew);
            double ta = a.Tone.LowPass(dl.Read(baseA + varA * (1 + a.Cur))) * a.Gain;
            double tb = hpB.HighPass(b.Tone.LowPass(dl.Read(baseB + varB * (1 + b.Cur)))) * b.Gain;
            double mid = 0.5 * (ta + tb);
            double wl = (mid + w * (ta - mid)) * 1.12, wr = (mid + w * (tb - mid)) * 1.12; // decorrelated takes sum ~3 dB lower
            double m = mix * fade;
            l[i] = (float)(l[i] * (1 - m) + wl * m);
            r[i] = (float)(r[i] * (1 - m) + wr * m);
        }
    }
}

/// <summary>Lush stereo tri-chorus.</summary>
public sealed class Chorus : StereoEffect
{
    DelayLine dl, dr; double fs, phase, fade, fadeStep;
    static readonly double[] VoiceSin = new double[6], VoiceCos = new double[6];
    static Chorus() { for (int v = 0; v < 6; v++) { VoiceSin[v] = Math.Sin(v * Math.PI / 3); VoiceCos[v] = Math.Cos(v * Math.PI / 3); } }
    public Chorus() : base("chorus", "TRI CHORUS",
        new Param("RATE", 0.3f, v => (Log(v, 0.08, 6)).ToString("0.00") + " Hz"), new Param("DEPTH", 0.5f), new Param("MIX", 0.5f, Fmt.Pct))
    { }

    public override void Prepare(double f)
    {
        fs = f; dl = new DelayLine((int)(fs * 0.03)); dr = new DelayLine((int)(fs * 0.03));
        fadeStep = 1 / (fs * 0.015); fade = Enabled ? 1 : 0; phase = 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        if (!Enabled && fade == 0) return;
        double rate = Log(Params[0].Value, 0.08, 6), depth = Params[1].Value * 0.0045 * fs, mix = Params[2].Value;
        double baseD = 0.0075 * fs, inc = 2 * Math.PI * rate / fs;
        for (int i = 0; i < l.Length; i++)
        {
            double t = Enabled ? 1 : 0;
            fade = fade < t ? Math.Min(t, fade + fadeStep) : Math.Max(t, fade - fadeStep);
            phase += inc; if (phase > 2 * Math.PI) phase -= 2 * Math.PI;
            dl.Write(l[i]); dr.Write(r[i]);
            double sn = Math.Sin(phase), cs = Math.Cos(phase), wl = 0, wr = 0;
            for (int v = 0; v < 6; v++)
            {
                double lfo = sn * VoiceCos[v] + cs * VoiceSin[v]; // sin(phase + v*60°)
                if ((v & 1) == 0) wl += dl.Read(baseD + depth * (0.5 + 0.5 * lfo));
                else wr += dr.Read(baseD + depth * (0.5 + 0.5 * lfo));
            }
            wl /= 3; wr /= 3;
            double m = mix * fade;
            l[i] = (float)(l[i] * (1 - 0.45 * m) + wl * 0.9 * m);
            r[i] = (float)(r[i] * (1 - 0.45 * m) + wr * 0.9 * m);
        }
    }
}

/// <summary>Six-stage stereo phaser with resonance.</summary>
public sealed class Phaser : StereoEffect
{
    readonly double[] zl = new double[6], zr = new double[6];
    double fs, phase, fbl, fbr, fade, fadeStep, al, ar;
    public Phaser() : base("phaser", "PHASE 6",
        new Param("RATE", 0.25f, v => (Log(v, 0.05, 8)).ToString("0.00") + " Hz"), new Param("DEPTH", 0.75f), new Param("RESO", 0.4f), new Param("MIX", 0.5f, Fmt.Pct))
    { }

    public override void Prepare(double f) { fs = f; Array.Clear(zl); Array.Clear(zr); fbl = fbr = 0; fadeStep = 1 / (fs * 0.015); fade = Enabled ? 1 : 0; }

    static double Chain(double x, double a, double[] z)
    {
        for (int s = 0; s < z.Length; s++) { double y = a * x + z[s]; z[s] = x - a * y; x = y; }
        return x;
    }

    static double ApCoef(double fs, double f) { double t = Math.Tan(Math.PI * f / fs); return (t - 1) / (t + 1); }

    public override void Process(Span<float> l, Span<float> r)
    {
        if (!Enabled && fade == 0) return;
        double rate = Log(Params[0].Value, 0.05, 8), depth = Params[1].Value, fb = Params[2].Value * 0.85, mix = Params[3].Value;
        double inc = 2 * Math.PI * rate / fs;
        for (int i = 0; i < l.Length; i++)
        {
            double t = Enabled ? 1 : 0;
            fade = fade < t ? Math.Min(t, fade + fadeStep) : Math.Max(t, fade - fadeStep);
            phase += inc; if (phase > 2 * Math.PI) phase -= 2 * Math.PI;
            if ((i & 7) == 0)
            {
                double ml = 0.5 + 0.5 * Math.Sin(phase), mr = 0.5 + 0.5 * Math.Cos(phase);
                al = ApCoef(fs, 180 * Math.Pow(4200 / 180.0, ml * depth)); ar = ApCoef(fs, 180 * Math.Pow(4200 / 180.0, mr * depth));
            }
            double yl = Chain(l[i] + fbl * fb, al, zl), yr = Chain(r[i] + fbr * fb, ar, zr);
            fbl = yl + 1e-20; fbr = yr + 1e-20;
            double m = mix * fade;
            l[i] = (float)(l[i] * (1 - 0.5 * m) + yl * 0.5 * m);
            r[i] = (float)(r[i] * (1 - 0.5 * m) + yr * 0.5 * m);
        }
    }
}

/// <summary>
/// Stereo tape/analog style delay: gliding time changes, wow &amp; flutter, filtered and saturated
/// repeats, ping-pong spread, ducking and tap tempo. Repeats ring out when switched off.
/// </summary>
public sealed class Delay : StereoEffect
{
    DelayLine dl, dr;
    OnePole lpL, lpR, lp2L, lp2R, hpL, hpR;
    double fs, tSm, ws, wcs = 1, fsn, fcs = 1, env, feed, envAtk, envRel;
    int quiet;
    long lastTap; readonly System.Diagnostics.Stopwatch clock = System.Diagnostics.Stopwatch.StartNew();
    public static double TimeMs(float v) => Log(v, 30, 1800);

    public Delay() : base("delay", "TAPE ECHO",
        new Param("TIME", 0.6f, v => Fmt.Ms(TimeMs(v))), new Param("FEEDBACK", 0.38f, Fmt.Pct), new Param("MIX", 0.3f, Fmt.Pct),
        new Param("TONE", 0.55f), new Param("MOD", 0.3f), new Param("SPREAD", 0.6f, Fmt.Pct), new Param("DUCK", 0.0f))
    { Wide = true; }

    public override bool HasTap => true;

    public override void Tap()
    {
        long now = clock.ElapsedMilliseconds;
        long dt = now - lastTap; lastTap = now;
        if (dt > 60 && dt < 1800)
            Params[0].Value = (float)(Math.Log(dt / 30.0) / Math.Log(1800 / 30.0));
    }

    public override void Prepare(double f)
    {
        fs = f; dl = new DelayLine((int)(fs * 1.9)); dr = new DelayLine((int)(fs * 1.9));
        hpL.SetLowPass(fs, 110); hpR.SetLowPass(fs, 110);
        tSm = TimeMs(Params[0].Value) * fs / 1000;
        envAtk = Coef(fs, 0.005); envRel = Coef(fs, 0.3);
        feed = Enabled ? 1 : 0; quiet = 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        if (!Enabled && feed == 0 && quiet > fs * 2) return; // fully decayed, sleep
        double target = TimeMs(Params[0].Value) * fs / 1000;
        double fb = Params[1].Value * 0.97, mix = Params[2].Value * 1.1, mod = Params[4].Value, s = Params[5].Value, duck = Params[6].Value;
        double tone = Log(Params[3].Value, 1200, 14000);
        lpL.SetLowPass(fs, tone); lpR.SetLowPass(fs, tone); lp2L.SetLowPass(fs, tone * 1.5); lp2R.SetLowPass(fs, tone * 1.5);
        double glide = Coef(fs, 0.12), feedStep = 1 / (fs * 0.02);
        double wc1 = Math.Cos(2 * Math.PI * 0.55 / fs), ws1 = Math.Sin(2 * Math.PI * 0.55 / fs);
        double fc1 = Math.Cos(2 * Math.PI * 5.7 / fs), fs1 = Math.Sin(2 * Math.PI * 5.7 / fs);
        { double n = Math.Sqrt(ws * ws + wcs * wcs); if (n > 0) { ws /= n; wcs /= n; } else wcs = 1; }   // renormalise per block
        { double n = Math.Sqrt(fsn * fsn + fcs * fcs); if (n > 0) { fsn /= n; fcs /= n; } else fcs = 1; }
        double wowD = mod * 0.0035 * fs, flutD = mod * 0.00035 * fs;
        double peak = 0;
        for (int i = 0; i < l.Length; i++)
        {
            double ft = Enabled ? 1 : 0;
            feed = feed < ft ? Math.Min(ft, feed + feedStep) : Math.Max(ft, feed - feedStep);
            tSm += glide * (target - tSm);
            double t1 = ws * wc1 + wcs * ws1; wcs = wcs * wc1 - ws * ws1; ws = t1;   // rotate wow phasor
            double t2 = fsn * fc1 + fcs * fs1; fcs = fcs * fc1 - fsn * fs1; fsn = t2; // rotate flutter phasor
            double m = wowD * ws + flutD * fsn;
            double rl = dl.Read(tSm + m), rr = dr.Read(tSm - m * 0.8 + 0.0007 * fs * s);

            // repeats get darker, thinner and gently saturated each pass
            double ql = Math.Tanh(1.15 * hpL.HighPass(lp2L.LowPass(lpL.LowPass(rl)))) / 1.15;
            double qr = Math.Tanh(1.15 * hpR.HighPass(lp2R.LowPass(lpR.LowPass(rr)))) / 1.15;

            double dryL = l[i], dryR = r[i], mono = 0.5 * (dryL + dryR);
            double inL = feed * ((1 - s) * dryL + s * mono), inR = feed * (1 - s) * dryR;
            dl.Write(inL + fb * ((1 - s) * ql + s * qr) + 1e-20);
            dr.Write(inR + fb * ((1 - s) * qr + s * ql) + 1e-20);

            double a = Math.Abs(mono);
            env += (a > env ? envAtk : envRel) * (a - env);
            double wet = mix / (1 + duck * 25 * env);
            l[i] = (float)(dryL + rl * wet);
            r[i] = (float)(dryR + rr * wet);
            peak = Math.Max(peak, Math.Abs(rl) + Math.Abs(rr));
        }
        quiet = peak < 1e-5 ? quiet + l.Length : 0;
    }
}

/// <summary>Octave-up pitch shifter (two crossfaded grains) used for the reverb shimmer.</summary>
sealed class OctaveUp
{
    readonly DelayLine d; readonly int grain; double phase;
    public OctaveUp(double fs) { grain = (int)(fs * 0.06); d = new DelayLine(grain + 8); }
    public double Process(double x)
    {
        d.Write(x);
        phase += 1.0 / grain; if (phase >= 1) phase -= 1;
        double p2 = phase + 0.5; if (p2 >= 1) p2 -= 1;
        double w1 = Math.Sin(Math.PI * phase), w2 = Math.Sin(Math.PI * p2);
        return d.Read((1 - phase) * grain + 2) * w1 * w1 + d.Read((1 - p2) * grain + 2) * w2 * w2;
    }
}

/// <summary>
/// Lush stereo hall: input diffusion, 8-line modulated feedback delay network with
/// Hadamard mixing and per-line damping, pre-delay and an octave-up shimmer loop.
/// </summary>
public sealed class Reverb : StereoEffect
{
    static readonly double[] BaseMs = { 31.7, 37.9, 41.3, 47.9, 53.1, 59.9, 67.3, 73.7 };
    static readonly double[] ModHz = { 0.31, 0.43, 0.57, 0.69, 0.83, 0.97, 1.09, 1.21 };
    static readonly int[] DiffSamples = { 142, 107, 379, 277 };
    DelayLine[] lines; DelayLine pre; DelayLine[] diff; int[] diffLen;
    OnePole[] damp; OnePole wetHpL, wetHpR, shimHp;
    readonly double[] v = new double[8], ms = new double[8], mc = new double[8], rc = new double[8], rs = new double[8], gains = new double[8];
    OctaveUp shim;
    double fs, scale, feed, shimFb, shimEnv;
    int quiet;

    public static double DecaySec(float v) => Log(v, 0.4, 25);

    public Reverb() : base("reverb", "CATHEDRAL",
        new Param("DECAY", 0.45f, v => DecaySec(v).ToString("0.0") + " s"), new Param("PREDELAY", 0.15f, v => (v * 200).ToString("0") + " ms"),
        new Param("SIZE", 0.6f), new Param("TONE", 0.5f), new Param("MOD", 0.4f), new Param("SHIMMER", 0f, Fmt.Pct), new Param("MIX", 0.3f, Fmt.Pct))
    { Wide = true; }

    public override void Prepare(double f)
    {
        fs = f;
        lines = new DelayLine[8]; damp = new OnePole[8];
        for (int i = 0; i < 8; i++) { lines[i] = new DelayLine((int)(BaseMs[i] * 1.75 * fs / 1000 + fs * 0.004)); ms[i] = 0; mc[i] = 0; }
        pre = new DelayLine((int)(fs * 0.21));
        diff = new DelayLine[4]; diffLen = new int[4];
        for (int i = 0; i < 4; i++) { diffLen[i] = (int)(DiffSamples[i] * fs / 44100); diff[i] = new DelayLine(diffLen[i] + 4); }
        wetHpL.SetLowPass(fs, 90); wetHpR.SetLowPass(fs, 90); shimHp.SetLowPass(fs, 400);
        shim = new OctaveUp(fs);
        scale = 0.45 + 1.2 * Params[2].Value;
        feed = Enabled ? 1 : 0; quiet = 0; shimFb = 0; shimEnv = 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        if (!Enabled && feed == 0 && quiet > fs * 3) return;
        double t60 = DecaySec(Params[0].Value), preD = Math.Max(1, Params[1].Value * 0.2 * fs);
        double targetScale = 0.45 + 1.2 * Params[2].Value;
        double dampHz = Log(Params[3].Value, 1500, 15000), modD = Params[4].Value * 0.0016 * fs;
        double shimAmt = Params[5].Value, mix = Params[6].Value * 1.2;
        for (int i = 0; i < 8; i++)
        {
            damp[i].SetLowPass(fs, dampHz);
            double len = BaseMs[i] * scale * fs / 1000;
            gains[i] = Math.Pow(10, -3 * len / (fs * t60));
        }
        double feedStep = 1 / (fs * 0.02), glide = 1 - Math.Exp(-1 / (fs * 0.25));
        for (int i = 0; i < 8; i++)
        {
            double nrm = Math.Sqrt(ms[i] * ms[i] + mc[i] * mc[i]);
            if (nrm > 0) { ms[i] /= nrm; mc[i] /= nrm; } else { ms[i] = Math.Sin(i * 0.7); mc[i] = Math.Cos(i * 0.7); }
            rc[i] = Math.Cos(2 * Math.PI * ModHz[i] / fs); rs[i] = Math.Sin(2 * Math.PI * ModHz[i] / fs);
        }
        double peak = 0;
        for (int n = 0; n < l.Length; n++)
        {
            double ft = Enabled ? 1 : 0;
            feed = feed < ft ? Math.Min(ft, feed + feedStep) : Math.Max(ft, feed - feedStep);
            scale += glide * (targetScale - scale);

            double x = 0.5 * (l[n] + r[n]) * feed;
            pre.Write(x);
            x = pre.Read(preD) + shimFb * shimAmt * 0.55;
            for (int k = 0; k < 4; k++) // input diffusion (Schroeder allpasses)
            {
                double d = diff[k].Read(diffLen[k]);
                double w = x + 0.62 * d;
                x = d - 0.62 * w;
                diff[k].Write(w);
            }

            double lenScale = scale * fs / 1000;
            for (int i = 0; i < 8; i++)
            {
                double t = ms[i] * rc[i] + mc[i] * rs[i]; mc[i] = mc[i] * rc[i] - ms[i] * rs[i]; ms[i] = t;
                double d = BaseMs[i] * lenScale + modD * (1 + ms[i]);
                v[i] = damp[i].LowPass(lines[i].Read(d)) * gains[i];
            }
            double outL = v[0] - v[2] + v[4] - v[6] + 0.5 * (v[1] + v[5]);
            double outR = v[1] - v[3] + v[5] - v[7] + 0.5 * (v[2] + v[6]);

            // 8-point fast Walsh-Hadamard mix (energy preserving)
            for (int h = 1; h < 8; h <<= 1)
                for (int i = 0; i < 8; i += h << 1)
                    for (int j = i; j < i + h; j++) { double a = v[j], b = v[j + h]; v[j] = a + b; v[j + h] = a - b; }
            for (int i = 0; i < 8; i++)
                lines[i].Write(v[i] * 0.35355339 + x * ((i & 1) == 0 ? 0.5 : -0.5) + 1e-20);

            // shimmer loop with self-limiting so long decays can't build up without bound
            double ao = Math.Abs(outL) + Math.Abs(outR);
            shimEnv += (ao > shimEnv ? 0.001 : 0.00005) * (ao - shimEnv);
            shimFb = shimAmt > 0 ? Math.Tanh(shimHp.HighPass(shim.Process(0.5 * (outL + outR)))) / (1 + 10 * shimEnv) : 0;

            double wl = wetHpL.HighPass(outL) * 0.6, wr = wetHpR.HighPass(outR) * 0.6;
            double dry = 1 - 0.25 * mix * feed;
            l[n] = (float)(l[n] * dry + wl * mix);
            r[n] = (float)(r[n] * dry + wr * mix);
            peak = Math.Max(peak, Math.Abs(wl) + Math.Abs(wr));
        }
        quiet = peak < 1e-5 ? quiet + l.Length : 0;
    }
}
