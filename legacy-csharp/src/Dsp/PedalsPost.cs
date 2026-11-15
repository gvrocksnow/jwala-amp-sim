using System;

namespace Jwala.Dsp;

// =====================================================================  POST (after the cab, stereo)

/// <summary>Helpers shared by the stereo effects.</summary>
static class Fx
{
    public static double Ramp(double v, double target, double step) => v < target ? Math.Min(v + step, target) : Math.Max(v - step, target);

    /// <summary>Schroeder allpass on a fixed whole-sample delay.</summary>
    public static double Allpass(DelayLine d, int len, double g, double x)
    {
        double z = d.Tap(len);
        double w = x + g * z;
        d.Write(w);
        return z - g * w;
    }
}

/// <summary>Small deterministic PRNG (xorshift64*).</summary>
sealed class Rng
{
    ulong s;
    public Rng(ulong seed) => s = seed;
    /// <summary>Seeds the way the takes and tape heads do: spread the seed's bits, never zero.</summary>
    public static Rng Spread(ulong seed) => new(unchecked(seed * 0x9E3779B97F4A7C15UL) | 1);
    public double NextDouble()
    {
        s ^= s >> 12; s ^= s << 25; s ^= s >> 27;
        return (unchecked(s * 0x2545F4914F6CDD1DUL) >> 11) / (double)(1UL << 53);
    }
}

/// <summary>
/// Stereo doubler: fakes a double-tracked part. Two synthetic takes of the signal, one a few
/// cents sharp and one a few cents flat, are retimed note by note inside the same DELAY window
/// (so neither side leads on average and the image stays centred), given their own wandering
/// level and tone, phase-scrambled, then spread left/right.
/// </summary>
public sealed class Doubler : StereoEffect
{
    /// <summary>
    /// One synthetic take: a micro-pitch-shifted, retimed, decorrelated copy.
    ///
    /// The read point sits lo + u * span samples behind the input. A constant detune makes u
    /// creep steadily; when it runs out of window it splices (short crossfade) to the far end at the
    /// spot where the waveform lines up best, so the splice is inaudible. Each picked note also
    /// splices the take to a fresh random position, which is what makes it early or late per note
    /// the way a second performance would be.
    /// </summary>
    sealed class Take
    {
        /// <summary>+1 plays sharp, -1 flat</summary>
        readonly double sign;
        double u, oldU;
        /// <summary>splice crossfade progress, 1 = no splice running</summary>
        double xf = 1, xfStep;
        double drift, driftMid, driftTarget, gain = 1, gainTarget = 1, tilt, tiltTarget, timer;
        OnePole split;
        readonly DelayLine[] ap = new DelayLine[3];
        readonly int[] apLen = new int[3];
        readonly Rng rng;

        public Take(ulong seed, double sign, double u) { this.sign = sign; this.u = oldU = u; rng = Rng.Spread(seed); }

        public void Prepare(double fs, double[] apMs)
        {
            for (int k = 0; k < 3; k++) { apLen[k] = Math.Max((int)(apMs[k] * fs / 1000), 1); ap[k] = new DelayLine(apLen[k] + 1); }
            split.SetLowPass(fs, 800);
        }

        /// <summary>Splice to position <paramref name="to"/> over 1 / step samples (ignored while a splice is still running).</summary>
        public void Jump(double to, double step)
        {
            if (xf >= 1) { oldU = u; u = to; xf = 0; xfStep = step; }
        }

        public double Tick(DelayLine dl, double fs, double lo, double span, double amt, double slew)
        {
            if (--timer <= 0)
            {
                // a new "intention" every 120-380 ms: tuning, picking strength and pick position wander
                driftTarget = rng.NextDouble() * 2 - 1;
                gainTarget = 1 + (rng.NextDouble() * 2 - 1) * 0.12 * amt;
                tiltTarget = (rng.NextDouble() * 2 - 1) * 0.2 * amt;
                timer = fs * (0.12 + 0.26 * rng.NextDouble());
            }
            // two cascaded smoothers: the detune glides, never steps
            driftMid += slew * (driftTarget - driftMid);
            drift += slew * (driftMid - drift);
            gain += slew * (gainTarget - gain);
            tilt += slew * (tiltTarget - tilt);

            // AMOUNT 50% = 7 cents either side of the original, wandering by another +-1.5
            double cents = amt * (14 * sign + 3 * drift);
            double du = -cents * 5.7762e-4 / span; // playing sharp = the delay shrinks
            u += du;
            if (xf < 1)
            {
                oldU += du;
                xf = Math.Min(xf + xfStep, 1);
                u = Math.Clamp(u, 0, 1);
            }
            else if (u < 0 || u > 1)
            {
                // out of window: restart at the other end, in phase with what is playing now
                double search = Math.Min(0.5 * span, 0.012 * fs);
                double from = u < 0 ? lo + span - search : lo;
                double to = Align(dl, lo + u * span, from, (int)search, (int)(0.0025 * fs));
                Jump(Math.Clamp((to - lo) / span, 0, 1), 1 / (0.012 * fs));
            }

            double y = dl.Read(lo + u * span);
            if (xf < 1)
            {
                double g = 0.5 - 0.5 * Math.Cos(Math.PI * xf);
                y = y * g + dl.Read(lo + oldU * span) * (1 - g);
            }
            // a short allpass chain (different per take) scrambles the phase, so the takes stop being
            // copies of each other and their mono sum does not comb
            for (int k = 0; k < 3; k++) y = Fx.Allpass(ap[k], apLen[k], 0.5, y);
            double low = split.LowPass(y);
            return (low * (1 - tilt) + (y - low) * (1 + tilt)) * gain;
        }
    }

    /// <summary>The delay in from..=from + search whose surrounding 2 * half samples best match those
    /// around delay cur (normalized correlation).</summary>
    static double Align(DelayLine dl, double cur, double from, int search, int half)
    {
        int c0 = (int)Math.Max(cur, half), f0 = (int)Math.Max(from, half);
        double Score(int off)
        {
            double c = 0, e = 1e-12;
            for (int m = 0; m < 2 * half; m += 2)
            {
                double a = dl.Tap(f0 + off + half - m);
                c += a * dl.Tap(c0 + half - m);
                e += a * a;
            }
            return c / Math.Sqrt(e);
        }
        double best = double.MinValue; int bestOff = 0;
        for (int off = 0; off <= search; off += 3) { double sc = Score(off); if (sc > best) { best = sc; bestOff = off; } }
        for (int off = Math.Max(bestOff - 2, 0); off <= Math.Min(bestOff + 2, search); off++) { double sc = Score(off); if (sc > best) { best = sc; bestOff = off; } }
        return f0 + bestOff;
    }

    DelayLine dl;
    readonly Take a = new(11, 1, 0.3), b = new(23, -1, 0.7);
    readonly Rng rng = new(0x5DEECE66D1CE4E5BUL);
    double span = 1, env, envSlow, hold, fs = 48000, fade, fadeStep;
    public static double DelayMs(float v) => Log(v, 6, 45);

    public Doubler() : base("doubler", "DOUBLER",
        new Param("AMOUNT", 0.5f, Fmt.Pct), new Param("DELAY", 0.35f, v => DelayMs(v).ToString("0") + " ms"),
        new Param("WIDTH", 1f, Fmt.Pct), new Param("MIX", 0.85f, Fmt.Pct))
    { }

    public override void Prepare(double f)
    {
        fs = f;
        dl = new DelayLine((int)(fs * 0.07));
        a.Prepare(fs, new[] { 0.61, 1.37, 2.29 });
        b.Prepare(fs, new[] { 0.77, 1.63, 2.63 });
        span = DelayMs(Params[1].Value) * fs / 1000;
        fadeStep = 1 / (fs * 0.02); fade = Enabled ? 1 : 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        bool enabled = Enabled;
        if (!enabled && fade == 0) return;
        double amt = Params[0].Value, spanTo = DelayMs(Params[1].Value) * fs / 1000, w = Params[2].Value, mix = Params[3].Value;
        // the takes never get closer than 4 ms to the input: a note is spotted on the way in and
        // both takes have moved to their new timing before it reaches them
        double lo = 0.004 * fs;
        double slew = Coef(fs, 0.22), spanSlew = Coef(fs, 0.08);
        double envRel = 1 - Coef(fs, 0.008), envSlowK = Coef(fs, 0.04);
        double spread = Math.Sqrt(amt);
        double jumpStep = 1 / (0.003 * fs);
        double t = enabled ? 1 : 0;
        for (int i = 0; i < l.Length; i++)
        {
            fade = Fx.Ramp(fade, t, fadeStep);
            span += spanSlew * (spanTo - span);
            double mono = 0.5 * (l[i] + (double)r[i]);
            dl.Write(mono);

            // pick attack: the level jumps well clear of its recent average
            env = Math.Max(Math.Abs(mono), env * envRel);
            envSlow += envSlowK * (env - envSlow);
            hold -= 1;
            if (hold <= 0 && env > 1.8 * envSlow + 1e-3)
            {
                hold = 0.07 * fs;
                a.Jump(0.5 + (rng.NextDouble() - 0.5) * spread, jumpStep);
                b.Jump(0.5 + (rng.NextDouble() - 0.5) * spread, jumpStep);
            }

            double ta = a.Tick(dl, fs, lo, span, amt, slew);
            double tb = b.Tick(dl, fs, lo, span, amt, slew);
            double mid = 0.5 * (ta + tb);
            double wl = mid + w * (ta - mid), wr = mid + w * (tb - mid);
            double m = mix * fade;
            l[i] = (float)(l[i] * (1 - m) + wl * m);
            r[i] = (float)(r[i] * (1 - m) + wr * m);
        }
    }
}

/// <summary>
/// Lush stereo tri-chorus: three voices a side, each swept by a slow LFO (RATE), a fast shallow
/// one and its own wander. The three LFO phases sit 120 degrees apart, so the pitch wobble of the
/// voices cancels in the sum (thick, not seasick), and the right side sweeps opposite to the left.
/// </summary>
public sealed class Chorus : StereoEffect
{
    /// <summary>Resting delay of each voice in ms (even = left, odd = right). Staggered, so the voices
    /// never comb at the same frequencies and the two sides differ even with the knobs at rest.</summary>
    static readonly double[] BaseMs = { 6.3, 7.1, 8.1, 9.2, 10.3, 11.4 };
    /// <summary>Widest sweep of the main LFO either side of rest, ms.</summary>
    const double SwingMs = 3.0;
    /// <summary>Peak detune (pitch ratio) of the main LFO at DEPTH 10: about 21 cents. Above ~0.6 Hz the sweep
    /// narrows as RATE rises to hold this, so fast settings shimmer instead of going seasick.</summary>
    const double Detune = 0.012;
    /// <summary>The second, fast LFO of a tri-chorus: a shallow shimmer riding on the slow sweep.</summary>
    const double FastHz = 5.6, FastDetune = 0.004;
    /// <summary>Per-voice slow wander (incommensurate rates), so the pattern never audibly repeats.</summary>
    static readonly double[] DriftHz = { 0.110, 0.093, 0.137, 0.127, 0.163, 0.151 };
    const double DriftMs = 0.35;
    /// <summary>The wet path is band-limited like a bucket-brigade line: lows stay dry, tight and centred,
    /// and the top is rolled off so the voices sit behind the dry signal instead of fizzing.</summary>
    const double LowHz = 140, HighHz = 8000;
    /// <summary>How far the dry signal drops at MIX 100%, and the level of each voice.</summary>
    const double DryDuck = 0.3, Voice = 0.5;
    /// <summary>The LFOs are evaluated every Ctrl samples; the delays glide linearly in between.</summary>
    const int Ctrl = 16;

    DelayLine dl, dr;
    OnePole hpL, hpR;
    Biquad lpL, lpR;
    double fs = 48000, phase, fastPhase, fade, fadeStep;
    readonly double[] driftPhase = new double[6];
    /// <summary>current delay of each voice in samples, and its per-sample glide</summary>
    readonly double[] d = new double[6], dStep = new double[6];
    int ctr;
    /// <summary>smoothed sweep widths (slow LFO, fast LFO, wander) in seconds</summary>
    readonly double[] swing = new double[3];

    public Chorus() : base("chorus", "TRI CHORUS",
        new Param("RATE", 0.3f, v => (Log(v, 0.08, 6)).ToString("0.00") + " Hz"), new Param("DEPTH", 0.5f), new Param("MIX", 0.5f, Fmt.Pct))
    { }

    /// <summary>Sweep of the slow and of the fast LFO, and the wander, in seconds either side of rest.</summary>
    static void Swings(double rate, double depth, Span<double> outp)
    {
        outp[0] = depth * Math.Min(SwingMs * 1e-3, Detune / (2 * Math.PI * rate));
        // the shimmer bows out as RATE itself gets fast
        outp[1] = depth * FastDetune / (2 * Math.PI * FastHz) / (1 + Math.Pow(rate / 2.5, 2));
        outp[2] = depth * DriftMs * 1e-3;
    }

    /// <summary>Where voice <paramref name="v"/> should be right now, in samples.</summary>
    double Target(int v)
    {
        double third = Math.PI / 3;
        // left voices at 0/120/240 degrees, right ones in between (= the left sweep inverted);
        // the fast LFO visits the voices in the opposite order
        double lfo = swing[0] * Math.Sin(phase + v * third)
                   + swing[1] * Math.Sin(fastPhase + ((v * 5) % 6) * third)
                   + swing[2] * Math.Sin(driftPhase[v]);
        return (BaseMs[v] * 1e-3 + lfo) * fs;
    }

    /// <summary>Advances the LFOs by one control period and aims every voice at its next position.</summary>
    void Retarget(double rate, double depth)
    {
        double dt = Ctrl / fs;
        // the sweep widths glide to a new knob setting: a jump would be heard as a pitch zip
        double k = Coef(fs / Ctrl, 0.15);
        Span<double> to = stackalloc double[3];
        Swings(rate, depth, to);
        for (int n = 0; n < 3; n++) swing[n] += k * (to[n] - swing[n]);
        phase = (phase + 2 * Math.PI * rate * dt) % (2 * Math.PI);
        fastPhase = (fastPhase + 2 * Math.PI * FastHz * dt) % (2 * Math.PI);
        for (int v = 0; v < 6; v++)
        {
            driftPhase[v] = (driftPhase[v] + 2 * Math.PI * DriftHz[v] * dt) % (2 * Math.PI);
            dStep[v] = (Target(v) - d[v]) / Ctrl;
        }
    }

    public override void Prepare(double f)
    {
        fs = f;
        dl = new DelayLine((int)(fs * 0.03)); dr = new DelayLine((int)(fs * 0.03));
        hpL = default; hpR = default; hpL.SetLowPass(fs, LowHz); hpR.SetLowPass(fs, LowHz);
        lpL = default; lpR = default; lpL.LowPass(fs, Math.Min(HighHz, 0.45 * fs), 0.6); lpR.LowPass(fs, Math.Min(HighHz, 0.45 * fs), 0.6);
        fadeStep = 1 / (fs * 0.015); fade = Enabled ? 1 : 0;
        phase = fastPhase = 0; ctr = 0;
        // spread out, so the voices do not all start at rest together
        double[] dp = { 0.0, 1.1, 2.3, 3.2, 4.4, 5.5 };
        Array.Copy(dp, driftPhase, 6);
        Swings(Log(Params[0].Value, 0.08, 6), Params[1].Value, swing);
        for (int v = 0; v < 6; v++) { d[v] = Target(v); dStep[v] = 0; }
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        bool enabled = Enabled;
        if (!enabled && fade == 0) return;
        if (fade == 0)
        {
            // switched on: nothing was recorded while bypassed, so start from silence
            dl.Clear(); dr.Clear();
            hpL.Reset(); hpR.Reset(); lpL.Reset(); lpR.Reset();
        }
        double rate = Log(Params[0].Value, 0.08, 6), depth = Params[1].Value, mix = Params[2].Value;
        double t = enabled ? 1 : 0;
        for (int i = 0; i < l.Length; i++)
        {
            fade = Fx.Ramp(fade, t, fadeStep);
            if (ctr == 0) Retarget(rate, depth);
            ctr = (ctr + 1) % Ctrl;

            double xl = l[i], xr = r[i];
            dl.Write(lpL.Process(hpL.HighPass(xl)));
            dr.Write(lpR.Process(hpR.HighPass(xr)));
            double wl = 0, wr = 0;
            for (int v = 0; v < 6; v++)
            {
                d[v] += dStep[v];
                if ((v & 1) == 0) wl += dl.Read(d[v]); else wr += dr.Read(d[v]);
            }
            double m = mix * fade;
            l[i] = (float)(xl * (1 - DryDuck * m) + wl * Voice * m);
            r[i] = (float)(xr * (1 - DryDuck * m) + wr * Voice * m);
        }
    }
}

/// <summary>
/// Lush stereo six-stage phaser. The two sides sweep in quadrature with their own slow wander,
/// the stages are slightly staggered, the sweep is exponential (equal time per octave) and
/// glides sample by sample, and the resonance runs through a filtered, saturating feedback loop.
/// </summary>
public sealed class Phaser : StereoEffect
{
    /// <summary>Allpass corner sweep at DEPTH 10, Hz. Six first-order stages give three notches,
    /// at about 0.27x, 1x and 3.7x the corner, so this sweeps the notches over roughly 60 Hz - 10 kHz.</summary>
    const double LoHz = 220, HiHz = 2600;
    /// <summary>Each stage sits a little off the others (in octaves), like the unmatched JFETs/caps of a real
    /// box: the notches come out broader and softer, a vocal swirl rather than a thin comb.</summary>
    static readonly double[] StaggerOct = { -0.21, -0.12, -0.04, 0.04, 0.12, 0.21 };
    /// <summary>Coefficients are computed every Ctrl samples and glide linearly in between (no zipper).</summary>
    const int Ctrl = 16;
    /// <summary>Slow wander of each side's sweep centre (incommensurate rates), so it never sounds looped.</summary>
    static readonly double[] WanderHz = { 0.071, 0.053 };
    const double WanderOct = 0.15;
    /// <summary>RESO at 10 sets this much feedback. The loop is band-limited (lows stay out so it never
    /// booms, the top is softened so the peaks sing instead of whistle) and soft-saturated.</summary>
    const double FbMax = 0.8, FbLowHz = 120, FbHighHz = 5500;
    /// <summary>The resonance peaks add level; the wet path gives it back by this much per unit of feedback.</summary>
    const double FbMakeup = 0.35;

    readonly double[][] z = { new double[6], new double[6] }, a = { new double[6], new double[6] }, aStep = { new double[6], new double[6] };
    readonly double[] fb = new double[2], wander = { 0, 2.1 };
    readonly OnePole[] fbHp = new OnePole[2], fbLp = new OnePole[2];
    double fs = 48000, phase, fade, fadeStep;
    int ctr;
    /// <summary>smoothed DEPTH, feedback amount and MIX</summary>
    double depthS, resoS, mixS;

    public Phaser() : base("phaser", "PHASE 6",
        new Param("RATE", 0.25f, v => (Log(v, 0.05, 8)).ToString("0.00") + " Hz"), new Param("DEPTH", 0.75f), new Param("RESO", 0.4f), new Param("MIX", 0.5f, Fmt.Pct))
    { }

    (double Rate, double Depth, double Reso, double Mix) Knobs() => (Log(Params[0].Value, 0.05, 8), Params[1].Value, Params[2].Value * FbMax, Params[3].Value);

    /// <summary>Allpass coefficients of side <paramref name="c"/> at the current LFO position.</summary>
    void Coefs(int c, double[] outp)
    {
        double half = 0.5 * Math.Log2(HiHz / LoHz);
        double centre = Math.Sqrt(LoHz * HiHz);
        // right side 90 degrees behind the left
        double lfo = Math.Sin(phase - c * 0.5 * Math.PI);
        double oct = depthS * (half * lfo + WanderOct * Math.Sin(wander[c]));
        for (int k = 0; k < 6; k++)
        {
            double f = Math.Min(centre * Math.Pow(2, oct + StaggerOct[k]), 0.45 * fs);
            double t = Math.Tan(Math.PI * f / fs);
            outp[k] = (t - 1) / (t + 1);
        }
    }

    readonly double[] tmp = new double[6];

    /// <summary>Advances the LFOs by one control period and aims the coefficients at their next values.</summary>
    void Retarget(double rate, double depth, double reso, double mix)
    {
        double dt = Ctrl / fs;
        double k = Coef(fs / Ctrl, 0.05);
        depthS += k * (depth - depthS);
        resoS += k * (reso - resoS);
        mixS += k * (mix - mixS);
        phase = (phase + 2 * Math.PI * rate * dt) % (2 * Math.PI);
        for (int c = 0; c < 2; c++)
        {
            wander[c] = (wander[c] + 2 * Math.PI * WanderHz[c] * dt) % (2 * Math.PI);
            Coefs(c, tmp);
            for (int s = 0; s < 6; s++) aStep[c][s] = (tmp[s] - a[c][s]) / Ctrl;
        }
    }

    void ResetState()
    {
        Array.Clear(z[0]); Array.Clear(z[1]); Array.Clear(fb);
        for (int c = 0; c < 2; c++) { fbHp[c].Reset(); fbLp[c].Reset(); }
    }

    public override void Prepare(double f)
    {
        fs = f;
        ResetState();
        for (int c = 0; c < 2; c++) { fbHp[c].SetLowPass(fs, FbLowHz); fbLp[c].SetLowPass(fs, Math.Min(FbHighHz, 0.4 * fs)); }
        fadeStep = 1 / (fs * 0.015); fade = Enabled ? 1 : 0;
        var (_, depth, reso, mix) = Knobs();
        depthS = depth; resoS = reso; mixS = mix;
        phase = 0; wander[0] = 0; wander[1] = 2.1; ctr = 0;
        for (int c = 0; c < 2; c++) { Coefs(c, a[c]); Array.Clear(aStep[c]); }
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        bool enabled = Enabled;
        if (!enabled && fade == 0) return;
        // switched on: start the loop from silence
        if (fade == 0) ResetState();
        var (rate, depth, reso, mix) = Knobs();
        double t = enabled ? 1 : 0;
        for (int i = 0; i < l.Length; i++)
        {
            fade = Fx.Ramp(fade, t, fadeStep);
            if (ctr == 0) Retarget(rate, depth, reso, mix);
            ctr = (ctr + 1) % Ctrl;

            double m = mixS * fade;
            double makeup = 1 / (1 + FbMakeup * resoS / FbMax);
            // dry and wet in random phase average to (1-m/2)^2 + (m/2)^2 of the power: give it back
            double level = 1 / Math.Sqrt(1 - m + 0.5 * m * m);
            for (int c = 0; c < 2; c++)
            {
                double x = c == 0 ? l[i] : r[i];
                double y = x + fb[c];
                double[] ac = a[c], st = aStep[c], zc = z[c];
                for (int s = 0; s < 6; s++)
                {
                    double ak = ac[s] + st[s];
                    ac[s] = ak;
                    double o = ak * y + zc[s];
                    zc[s] = y - ak * o;
                    y = o;
                }
                // band-limited, soft-saturated resonance
                double fv = fbLp[c].LowPass(fbHp[c].HighPass(y));
                fb[c] = Math.Tanh(resoS * fv) + 1e-20;
                float outv = (float)((x * (1 - 0.5 * m) + y * 0.5 * m * makeup) * level);
                if (c == 0) l[i] = outv; else r[i] = outv;
            }
        }
    }
}

/// <summary>
/// Stereo tape style delay: gliding time changes, wow, flutter and random speed drift (independent
/// per side), repeats that pass through a tape head model (TapeHead) and bloom into a wash as
/// DIFFUSE comes up, ping-pong spread, ducking and tap tempo. Repeats ring out when switched off.
/// </summary>
public sealed class Delay : StereoEffect
{
    /// <summary>Diffusion allpass lengths in ms, left then right: mutually prime-ish and different on each side,
    /// so the smear also decorrelates the two channels.</summary>
    static readonly double[][] ApMs = { new[] { 2.3, 3.7, 6.1, 9.7 }, new[] { 2.9, 4.3, 5.3, 9.1 } };
    const double TapeDrive = 1.15;
    /// <summary>Bias into the tape curve: a little asymmetry, so the repeats pick up even harmonics too.</summary>
    const double TapeBias = 0.15;
    /// <summary>High-frequency pre-emphasis around the tape curve (gain above ~2.5 kHz is 1 + this).</summary>
    const double TapeEmph = 1.6;

    /// <summary>Slow random wander of the tape speed, one per channel.</summary>
    sealed class Drift
    {
        double cur, mid, target, timer;
        readonly Rng rng;
        public Drift(ulong seed) => rng = Rng.Spread(seed);
        public double Step(double fs, double slew)
        {
            if (--timer <= 0)
            {
                target = rng.NextDouble() * 2 - 1;
                timer = fs * (0.4 + 0.8 * rng.NextDouble());
            }
            // two cascaded smoothers: the speed (= pitch) glides, it never steps
            mid += slew * (target - mid);
            cur += slew * (mid - cur);
            return cur;
        }
    }

    /// <summary>
    /// One channel of the echo's playback path. Everything read off the delay line goes through it
    /// before it is heard or fed back, so the first repeat is already "tape" and each further pass
    /// gets darker, softer and more diffuse: tone filters, low cut with a small head bump, an
    /// asymmetric tape curve inside a pre/de-emphasis pair (loud highs compress first) and four
    /// allpass diffusers.
    /// </summary>
    sealed class TapeHead
    {
        OnePole lp, lp2, hp;
        Biquad bump;
        double emphA, preZ, deZ;
        readonly double satOff, satNorm;
        readonly DelayLine[] aps = new DelayLine[4];
        readonly int[] apLen = { 1, 1, 1, 1 };
        /// <summary>Total allpass delay in samples; the main line is shortened by it so TIME stays true.</summary>
        public double ApSum = 4;
        public readonly Drift Drift;

        public TapeHead(ulong seed)
        {
            satOff = Math.Tanh(TapeBias);
            satNorm = 1 / (TapeDrive * (1 - satOff * satOff));
            Drift = new Drift(seed);
        }

        public void Prepare(double fs, int side)
        {
            hp.SetLowPass(fs, 95);
            // with the 95 Hz low cut the net stays below unity everywhere, so the loop cannot run away
            bump.Peak(fs, 105, 0.8, 1.5);
            emphA = 1 - Math.Exp(-2 * Math.PI * 2500 / fs);
            ApSum = 0;
            for (int k = 0; k < 4; k++)
            {
                apLen[k] = Math.Max((int)(ApMs[side][k] * fs / 1000), 1);
                aps[k] = new DelayLine(apLen[k] + 4);
                ApSum += apLen[k];
            }
        }

        public void SetTone(double fs, double tone) { lp.SetLowPass(fs, tone); lp2.SetLowPass(fs, tone * 1.5); }

        public double Tick(double x, double g)
        {
            x = lp2.LowPass(lp.LowPass(x));
            x = bump.Process(hp.HighPass(x));
            // pre-emphasis, tape curve, exactly inverse de-emphasis: transparent when quiet
            double k = TapeEmph * (1 - emphA);
            double e = x * (1 + k) - k * preZ;
            preZ += emphA * (x - preZ);
            double s = (Math.Tanh(TapeDrive * e + TapeBias) - satOff) * satNorm;
            double y = (s + k * deZ) / (1 + k);
            deZ += emphA * (y - deZ);
            for (int n = 0; n < 4; n++) y = Fx.Allpass(aps[n], apLen[n], g, y);
            return y;
        }
    }

    DelayLine dl, dr;
    readonly TapeHead hl = new(41), hr = new(67);
    double fs = 48000, tSm, gSm, ws, wcs = 1, fsn, fcs = 1, env, feed, envAtk, envRel;
    long quiet;
    long lastTap; readonly System.Diagnostics.Stopwatch clock = System.Diagnostics.Stopwatch.StartNew();
    public static double TimeMs(float v) => Log(v, 30, 1800);
    /// <summary>DIFFUSE knob to allpass coefficient.</summary>
    static double Diffusion(float v) => v * 0.7;

    public Delay() : base("delay", "TAPE ECHO",
        new Param("TIME", 0.6f, v => Fmt.Ms(TimeMs(v))), new Param("FEEDBACK", 0.38f, Fmt.Pct), new Param("MIX", 0.3f, Fmt.Pct),
        new Param("TONE", 0.55f), new Param("MOD", 0.3f), new Param("SPREAD", 0.6f, Fmt.Pct), new Param("DUCK", 0.0f),
        new Param("DIFFUSE", 0.4f))
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
        fs = f;
        dl = new DelayLine((int)(fs * 1.9)); dr = new DelayLine((int)(fs * 1.9));
        hl.Prepare(fs, 0); hr.Prepare(fs, 1);
        tSm = TimeMs(Params[0].Value) * fs / 1000;
        gSm = Diffusion(Params[7].Value);
        envAtk = Coef(fs, 0.005); envRel = Coef(fs, 0.3);
        feed = Enabled ? 1 : 0; quiet = 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        bool enabled = Enabled;
        if (!enabled && feed == 0 && quiet > fs * 2) return; // fully decayed, sleep
        double target = TimeMs(Params[0].Value) * fs / 1000;
        double fb = Params[1].Value * 0.97, mix = Params[2].Value * 1.1, md = Params[4].Value, s = Params[5].Value, duck = Params[6].Value;
        double gTarget = Diffusion(Params[7].Value);
        double tone = Log(Params[3].Value, 1200, 14000);
        hl.SetTone(fs, tone); hr.SetTone(fs, tone);
        double glide = Coef(fs, 0.12), feedStep = 1 / (fs * 0.02), driftSlew = Coef(fs, 0.35);
        double wc1 = Math.Cos(2 * Math.PI * 0.55 / fs), ws1 = Math.Sin(2 * Math.PI * 0.55 / fs);
        double fc1 = Math.Cos(2 * Math.PI * 5.7 / fs), fs1 = Math.Sin(2 * Math.PI * 5.7 / fs);
        // renormalise the LFO phasors per block
        { double n = Math.Sqrt(ws * ws + wcs * wcs); if (n > 0) { ws /= n; wcs /= n; } else wcs = 1; }
        { double n = Math.Sqrt(fsn * fsn + fcs * fcs); if (n > 0) { fsn /= n; fcs /= n; } else fcs = 1; }
        double wowD = md * 0.003 * fs, flutD = md * 0.0003 * fs, driftD = md * 0.0025 * fs;
        double ft = enabled ? 1 : 0;
        double peak = 0;
        for (int i = 0; i < l.Length; i++)
        {
            feed = Fx.Ramp(feed, ft, feedStep);
            tSm += glide * (target - tSm);
            gSm += glide * (gTarget - gSm);
            double t1 = ws * wc1 + wcs * ws1; wcs = wcs * wc1 - ws * ws1; ws = t1;   // rotate wow phasor
            double t2 = fsn * fc1 + fcs * fs1; fcs = fcs * fc1 - fsn * fs1; fsn = t2; // rotate flutter phasor
            // the right side runs a quarter cycle behind and has its own drift: the image moves
            // instead of the two sides pulling against each other
            double ml = wowD * ws + flutD * fsn + driftD * hl.Drift.Step(fs, driftSlew);
            double mr = 0.9 * (wowD * wcs + flutD * fcs) + driftD * hr.Drift.Step(fs, driftSlew);
            double rl = dl.Read(tSm - hl.ApSum + ml);
            double rr = dr.Read(tSm - hr.ApSum + mr + 0.0007 * fs * s);
            double ql = hl.Tick(rl, gSm), qr = hr.Tick(rr, gSm);

            double dryL = l[i], dryR = r[i], mono = 0.5 * (dryL + dryR);
            double inL = feed * ((1 - s) * dryL + s * mono), inR = feed * (1 - s) * dryR;
            dl.Write(inL + fb * ((1 - s) * ql + s * qr) + 1e-20);
            dr.Write(inR + fb * ((1 - s) * qr + s * ql) + 1e-20);

            double am = Math.Abs(mono);
            env += (am > env ? envAtk : envRel) * (am - env);
            double wet = mix / (1 + duck * 25 * env);
            l[i] = (float)(dryL + ql * wet);
            r[i] = (float)(dryR + qr * wet);
            peak = Math.Max(peak, Math.Abs(ql) + Math.Abs(qr));
        }
        quiet = peak < 1e-5 ? quiet + l.Length : 0;
    }
}

/// <summary>
/// Octave-up pitch shifter for the reverb shimmer. Two crossfaded grains read the input at double
/// speed; each new grain starts from the point (within a 25 ms search) where the waveform best
/// matches the grain already playing, so the two stay in phase and the octave comes out steady
/// instead of warbling at the grain rate.
/// </summary>
sealed class OctaveUp
{
    readonly DelayLine d;
    readonly double grain;
    readonly int search, span;
    double phase;
    /// <summary>cos/sin of 2*pi*phase, kept by rotation so the grain windows need no trig per sample</summary>
    double winC = 1, winS;
    readonly double rotC, rotS;
    readonly double[] off = new double[2];

    public OctaveUp(double fs, double grainS)
    {
        grain = fs * grainS; search = (int)(fs * 0.025);
        d = new DelayLine((int)grain + search + 8);
        span = (int)Math.Min(grain * 0.4, 512);
        rotC = Math.Cos(2 * Math.PI / grain); rotS = Math.Sin(2 * Math.PI / grain);
    }

    public double Process(double x)
    {
        d.Write(x);
        double step = 1 / grain;
        phase += step;
        (winC, winS) = (winC * rotC - winS * rotS, winS * rotC + winC * rotS);
        if (phase >= 1)
        {
            phase -= 1;
            winC = Math.Cos(2 * Math.PI * phase); winS = Math.Sin(2 * Math.PI * phase);
        }
        double y = 0;
        for (int k = 0; k < 2; k++)
        {
            double ph = phase + 0.5 * k;
            if (ph >= 1) ph -= 1;
            // the read point closes in by one sample per sample = double speed = one octave up
            double baseD = (1 - ph) * grain + 2;
            // this grain is silent right now and the other one is at full level: restart in phase with it
            if (ph < step) off[k] = Align(baseD, baseD - 0.5 * grain + off[1 - k]);
            // Hann windows half a grain apart: they always add up to one
            double w = k == 0 ? 0.5 - 0.5 * winC : 0.5 + 0.5 * winC;
            y += d.Read(baseD + off[k]) * w;
        }
        return y;
    }

    /// <summary>Extra delay for a grain starting at delay <paramref name="baseD"/> that makes what it is about to play line up
    /// with what the grain at delay <paramref name="other"/> is about to play (best normalized correlation).</summary>
    double Align(double baseD, double other)
    {
        int a0 = (int)baseD, b0 = (int)other;
        double Score(int o)
        {
            double c = 0, e = 1e-12;
            for (int m = 0; m < span; m += 4)
            {
                double a = d.Tap(a0 + o - m);
                c += a * d.Tap(b0 - m);
                e += a * a;
            }
            return c / Math.Sqrt(e);
        }
        double best = double.MinValue; int bestOff = 0;
        for (int o = 0; o <= search; o += 4) { double sc = Score(o); if (sc > best) { best = sc; bestOff = o; } }
        for (int o = Math.Max(bestOff - 3, 0); o <= Math.Min(bestOff + 3, search); o++) { double sc = Score(o); if (sc > best) { best = sc; bestOff = o; } }
        return bestOff;
    }
}

/// <summary>
/// Lush stereo hall. Stereo in, per-channel pre-delay and input diffusion, then a 16-line
/// feedback delay network (Hadamard mixing) with an allpass diffuser, high damping and a separate
/// low-band decay in every line; line lengths wander with a sine plus a slow random drift.
/// Shimmer: the tank output goes through a stereo octave-up shifter and back into the tank, so
/// every trip around adds another, softer octave.
/// </summary>
public sealed class Reverb : StereoEffect
{
    const int Tank = 16;
    static readonly double[] LineMs = { 23.3, 27.1, 29.9, 33.7, 37.3, 41.9, 44.3, 48.1, 53.3, 57.7, 61.9, 67.1, 71.3, 77.9, 83.9, 89.3 };
    static readonly double[] TankApMs = { 5.3, 3.7, 7.1, 4.3, 8.9, 6.1, 10.7, 4.9, 11.9, 7.7, 9.5, 5.9, 12.7, 8.3, 6.7, 10.1 };
    static readonly double[] ModHz = { 0.19, 0.27, 0.31, 0.37, 0.43, 0.49, 0.57, 0.63, 0.69, 0.77, 0.83, 0.91, 0.97, 1.03, 1.09, 1.21 };
    /// <summary>Input diffusers: four for the left input, then four for the right (samples at 44.1 kHz).</summary>
    static readonly int[] DiffSamples = { 142, 107, 379, 277, 151, 113, 359, 293 };
    /// <summary>Diffusers on the shimmer return: two left, two right.</summary>
    static readonly double[] ShimDiffMs = { 13.7, 19.3, 15.1, 21.7 };
    const double TankApG = 0.5, ReverbOut = 0.66, ShimK = 0.8, ShimLimit = 0.3;

    DelayLine[] lines, aps, pre, diff, sdiff;
    readonly int[] apLen = new int[Tank], diffLen = new int[8], sdiffLen = new int[4];
    OnePole[] damp = new OnePole[Tank], bass = new OnePole[Tank];
    readonly double[] gains = new double[Tank], shelf = new double[Tank], v = new double[Tank],
        ms = new double[Tank], mc = new double[Tank], rc = new double[Tank], rs = new double[Tank],
        rndTarget = new double[Tank], rndMid = new double[Tank], rnd = new double[Tank], rndTimer = new double[Tank];
    readonly Rng rng = new(0x5EEDCA7ED2A10001UL);
    OnePole[] wetHp = new OnePole[2], shimHp = new OnePole[2], shimLp = new OnePole[2];
    OctaveUp[] shim;
    readonly double[] shimFb = new double[2];
    double shimEnv, fs = 48000, scale = 1, feed;
    long quiet;

    public static double DecaySec(float v) => Log(v, 0.4, 25);

    public Reverb() : base("reverb", "CATHEDRAL",
        new Param("DECAY", 0.45f, v => DecaySec(v).ToString("0.0") + " s"), new Param("PREDELAY", 0.15f, v => (v * 200).ToString("0") + " ms"),
        new Param("SIZE", 0.6f), new Param("TONE", 0.5f), new Param("MOD", 0.4f), new Param("SHIMMER", 0f, Fmt.Pct), new Param("MIX", 0.3f, Fmt.Pct))
    { Wide = true; }

    public override void Prepare(double f)
    {
        fs = f;
        lines = new DelayLine[Tank]; aps = new DelayLine[Tank];
        for (int i = 0; i < Tank; i++)
        {
            lines[i] = new DelayLine((int)(LineMs[i] * 1.75 * fs / 1000 + fs * 0.004));
            apLen[i] = (int)(TankApMs[i] * fs / 1000);
            aps[i] = new DelayLine(apLen[i] + 4);
        }
        pre = new[] { new DelayLine((int)(fs * 0.21)), new DelayLine((int)(fs * 0.21)) };
        diff = new DelayLine[8];
        for (int i = 0; i < 8; i++) { diffLen[i] = (int)(DiffSamples[i] * fs / 44100); diff[i] = new DelayLine(diffLen[i] + 4); }
        sdiff = new DelayLine[4];
        for (int i = 0; i < 4; i++) { sdiffLen[i] = (int)(ShimDiffMs[i] * fs / 1000); sdiff[i] = new DelayLine(sdiffLen[i] + 4); }
        shim = new[] { new OctaveUp(fs, 0.089), new OctaveUp(fs, 0.101) };
        damp = new OnePole[Tank]; bass = new OnePole[Tank];
        wetHp = new OnePole[2]; shimHp = new OnePole[2]; shimLp = new OnePole[2];
        for (int c = 0; c < 2; c++) { wetHp[c].SetLowPass(fs, 90); shimHp[c].SetLowPass(fs, 250); shimLp[c].SetLowPass(fs, 6000); }
        for (int i = 0; i < Tank; i++) bass[i].SetLowPass(fs, 250);
        Array.Clear(ms); Array.Clear(mc); Array.Clear(rndTarget); Array.Clear(rndMid); Array.Clear(rnd); Array.Clear(rndTimer);
        scale = 0.45 + 1.2 * Params[2].Value;
        feed = Enabled ? 1 : 0; quiet = 0;
        Array.Clear(shimFb); shimEnv = 0;
    }

    public override void Process(Span<float> l, Span<float> r)
    {
        bool enabled = Enabled;
        if (!enabled && feed == 0 && quiet > fs * 3) return;
        double p0 = Params[0].Value;
        double t60 = DecaySec(Params[0].Value);
        // lows ring a little longer than the mids in short rooms, shorter in huge ones (warm, never muddy)
        double t60Low = t60 * (1.35 - 0.6 * p0);
        double preD = Math.Max(Params[1].Value * 0.2 * fs, 1);
        double targetScale = 0.45 + 1.2 * Params[2].Value;
        double dampHz = Log(Params[3].Value, 1500, 15000), modD = Params[4].Value * 0.0016 * fs;
        double shimAmt = Params[5].Value, mix = Params[6].Value * 1.2;
        for (int i = 0; i < Tank; i++)
        {
            damp[i].SetLowPass(fs, dampHz);
            double len = LineMs[i] * scale * fs / 1000 + apLen[i];
            gains[i] = Math.Pow(10, -3 * len / (fs * t60));
            shelf[i] = Math.Pow(10, -3 * len / (fs * t60Low)) / gains[i] - 1;
        }
        // the tank gets louder as the decay gets longer: scale the shimmer send to match, so the knob feels the same at any decay
        double shimGain = shimAmt * ShimK * Math.Sqrt(scale / t60);
        double feedStep = 1 / (fs * 0.02), glide = 1 - Math.Exp(-1 / (fs * 0.25));
        double rndSlew = 1 - Math.Exp(-1 / (fs * 0.12));
        double envAtk = 1 - Math.Exp(-1 / (fs * 0.005)), envRel = 1 - Math.Exp(-1 / (fs * 0.3));
        for (int i = 0; i < Tank; i++)
        {
            double nrm = Math.Sqrt(ms[i] * ms[i] + mc[i] * mc[i]);
            if (nrm > 0) { ms[i] /= nrm; mc[i] /= nrm; } else { ms[i] = Math.Sin(i * 0.7); mc[i] = Math.Cos(i * 0.7); }
            rc[i] = Math.Cos(2 * Math.PI * ModHz[i] / fs); rs[i] = Math.Sin(2 * Math.PI * ModHz[i] / fs);
        }
        double ft = enabled ? 1 : 0;
        double peak = 0;
        Span<double> x = stackalloc double[2], outp = stackalloc double[2], s = stackalloc double[2];
        for (int n = 0; n < l.Length; n++)
        {
            feed = Fx.Ramp(feed, ft, feedStep);
            scale += glide * (targetScale - scale);

            // pre-delay, shimmer return and input diffusion, per channel
            x[0] = l[n] * feed; x[1] = r[n] * feed;
            for (int c = 0; c < 2; c++)
            {
                pre[c].Write(x[c]);
                x[c] = pre[c].Read(preD) + shimFb[c];
                for (int k = 0; k < 4; k++) x[c] = Fx.Allpass(diff[4 * c + k], diffLen[4 * c + k], 0.62, x[c]);
            }

            double lenScale = scale * fs / 1000;
            outp[0] = outp[1] = 0;
            for (int i = 0; i < Tank; i++)
            {
                double t = ms[i] * rc[i] + mc[i] * rs[i];
                mc[i] = mc[i] * rc[i] - ms[i] * rs[i];
                ms[i] = t;
                if (--rndTimer[i] <= 0)
                {
                    rndTarget[i] = rng.NextDouble() * 2 - 1;
                    rndTimer[i] = fs * (0.2 + 0.5 * rng.NextDouble());
                }
                rndMid[i] += rndSlew * (rndTarget[i] - rndMid[i]);
                rnd[i] += rndSlew * (rndMid[i] - rnd[i]);

                double dd = LineMs[i] * lenScale + modD * (1 + 0.6 * ms[i] + 0.4 * rnd[i]);
                double y = damp[i].LowPass(lines[i].Read(dd)) * gains[i];
                y += shelf[i] * bass[i].LowPass(y);
                y = Fx.Allpass(aps[i], apLen[i], TankApG, y);
                v[i] = y;
                // even lines feed the left output, odd lines the right
                outp[i & 1] += (i & 2) == 0 ? y : -y;
            }

            // 16-point fast Walsh-Hadamard mix (energy preserving)
            for (int h = 1; h < Tank; h <<= 1)
                for (int i = 0; i < Tank; i += h << 1)
                    for (int j = i; j < i + h; j++) { double a = v[j], b = v[j + h]; v[j] = a + b; v[j + h] = a - b; }
            for (int i = 0; i < Tank; i++)
            {
                double inj = x[i & 1] * ((i & 4) == 0 ? 0.35 : -0.35);
                lines[i].Write(v[i] * 0.25 + inj + 1e-20);
            }

            // shimmer: octave-up of each side, band-limited and diffused, returns to the opposite side
            if (shimAmt > 0)
            {
                for (int c = 0; c < 2; c++)
                {
                    double y = shim[c].Process(outp[c]);
                    y = shimLp[c].LowPass(shimHp[c].HighPass(y));
                    y = Fx.Allpass(sdiff[2 * c], sdiffLen[2 * c], 0.6, y);
                    s[c] = shimGain * Fx.Allpass(sdiff[2 * c + 1], sdiffLen[2 * c + 1], 0.6, y);
                }
                // transparent until the return gets loud, then it is held at a ceiling
                double a = Math.Max(Math.Abs(s[0]), Math.Abs(s[1]));
                shimEnv += (a > shimEnv ? envAtk : envRel) * (a - shimEnv);
                double g = ShimLimit / Math.Max(shimEnv, ShimLimit);
                shimFb[0] = 2 * Math.Tanh(0.5 * g * s[1]);
                shimFb[1] = 2 * Math.Tanh(0.5 * g * s[0]);
            }
            else shimFb[0] = shimFb[1] = 0;

            double wl = wetHp[0].HighPass(outp[0]) * ReverbOut;
            double wr = wetHp[1].HighPass(outp[1]) * ReverbOut;
            double dry = 1 - 0.25 * mix * feed;
            l[n] = (float)(l[n] * dry + wl * mix);
            r[n] = (float)(r[n] * dry + wr * mix);
            peak = Math.Max(peak, Math.Abs(wl) + Math.Abs(wr));
        }
        quiet = peak < 1e-5 ? quiet + l.Length : 0;
    }
}
