using System;
using System.Numerics;

namespace Jwala.Dsp;

/// <summary>
/// The amp's ACOUSTIC channel: turns a magnetic-pickup DI into a miked acoustic guitar.
/// Pickup correction → "sparkle" exciter → modal body IR → air/top resonances → EQ.
/// The body's spectral envelope was tone-matched (1/3-octave LTAS, octave-smoothed below
/// 250 Hz where the notes played dominate) to a steel-string dreadnought recording (Ibanez
/// AAD300CE), played from a real magnetic-pickup DI.
///
/// Knob meanings on this channel: GAIN = BODY (how much of the body's ringing tail
/// is heard; low = drier, piezo-like), BASS/MIDDLE/TREBLE = EQ (centre = matched),
/// PRESENCE = AIR (sparkle + top shelf), DEPTH = RESO (air-cavity/top-plate
/// resonances), MASTER = LEVEL.
/// </summary>
public sealed class AcousticSim
{
    /// <summary>Body spectral envelope in dB at the 1/3-octave centres 20 Hz · 2^(i/3), i = 0..30.</summary>
    static readonly double[] BodyDb =
    {
        26.0, 26.0, 26.0, 26.0, 26.0, 26.0, 26.0, 19.1, 15.8, -4.8, 11.9, 21.2, 14.7, 6.6, 18.7, -10.5, -8.4, -1.9, 2.4, -4.5,
        -0.7, -0.4, 1.2, -11.1, -8.2, -11.8, -9.6, -10.5, -4.9, 0.4, 0.4,
    };

    /// <summary>Loudness make-up so the channel sits near the clean channel's level.</summary>
    const double Makeup = 29.6;
    const int MaxTaps = 2048;

    static double Db(double db) => Math.Pow(10, db / 20);

    /// <summary>xorshift32; the body IR must come out identical in the Rust version.</summary>
    struct Rng
    {
        uint x;
        public Rng(uint seed) => x = seed;
        public double Next()
        {
            x ^= x << 13; x ^= x >> 17; x ^= x << 5;
            return (x >> 8) / 16777216.0;
        }
    }

    /// <summary>Envelope gain (dB) at f, interpolated on a log-frequency axis, clamped at the ends.</summary>
    static double BodyGain(double f)
    {
        double pos = 3 * Math.Log2(f / 20);
        if (pos <= 0) return BodyDb[0];
        int i = (int)Math.Floor(pos);
        if (i >= BodyDb.Length - 1) return BodyDb[^1];
        double t = pos - i;
        return BodyDb[i] + (BodyDb[i + 1] - BodyDb[i]) * t;
    }

    /// <summary>
    /// The body's impulse response: a sum of exponentially decaying modes, sparse and
    /// long-ringing in the lows, dense and short in the highs. Each mode's amplitude is
    /// set so the power spectrum follows BodyDb whatever the mode density and decay.
    /// </summary>
    public static float[] BodyIr(double fs)
    {
        int n = Math.Min(MaxTaps, (int)(fs * 0.045));
        var ir = new double[n];
        var rng = new Rng(0x2F6B9A1D);
        double top = Math.Min(18000, 0.45 * fs);
        double f = 70;
        while (f < top)
        {
            double spacing = Math.Max(0.30 * Math.Pow(f / 70, -0.75), 0.018);
            double df = f * spacing;
            double fk = f * (1 + (rng.Next() - 0.5) * spacing);
            double t60 = Math.Min(fk < 250 ? 0.040 : 1.0, 0.06 * Math.Pow(fk / 250, -0.55));
            double tau = t60 / 6.91;
            double amp = Math.Sqrt(df / tau) * Db(BodyGain(fk)) * (0.55 + 0.9 * rng.Next());
            double sign = rng.Next() < 0.5 ? 1 : -1;
            // decaying sine by recursion: rotate a phasor and shrink it every sample
            double w = 2 * Math.PI * fk / fs, d = Math.Exp(-1 / (fs * tau));
            double c = Math.Cos(w) * d, s = Math.Sin(w) * d;
            double re = sign * amp, im = 0;
            for (int i = 0; i < n; i++)
            {
                ir[i] += im;
                double r2 = re * c - im * s;
                im = re * s + im * c;
                re = r2;
            }
            f += df;
        }
        var outIr = new float[n];
        for (int i = 0; i < n; i++) outIr[i] = (float)ir[i];
        Convolver.Normalize(outIr, fs);
        return outIr;
    }

    /// <summary>
    /// Direct-form FIR (SIMD) that returns the body IR's first 2.5 ms ("head") and the rest
    /// ("tail") separately, so BODY can scale the ringing without changing the attack.
    /// </summary>
    sealed class BodyFir
    {
        readonly float[] head, tail, hist;
        int pos;

        public BodyFir(float[] ir, double fs)
        {
            int w = Vector<float>.Count;
            int len = Math.Clamp(ir.Length, 1, MaxTaps);
            int padded = (len + w - 1) / w * w;
            int headLen = Math.Min(((int)(fs * 0.0025) + 1 + w - 1) / w * w, padded);
            head = new float[headLen]; tail = new float[padded]; hist = new float[padded * 2];
            for (int i = 0; i < len; i++)
            {
                float fade = i + 64 > len ? (len - i) / 64f : 1f;
                // tail weight: 0 before 0.5 ms, raised cosine up to 1 at 2.5 ms
                double u = Math.Clamp((i / fs - 0.0005) / 0.002, 0, 1);
                float tw = (float)(0.5 - 0.5 * Math.Cos(Math.PI * u));
                if (i < headLen) head[headLen - 1 - i] = ir[i] * fade * (1 - tw);
                tail[padded - 1 - i] = ir[i] * fade * tw;
            }
        }

        public (float Head, float Tail) Process(float x)
        {
            int n = tail.Length, w = Vector<float>.Count;
            pos = (pos + 1) % n;
            hist[pos] = x; hist[pos + n] = x;
            var hs = new ReadOnlySpan<float>(hist, pos + 1, n);
            var b = Vector<float>.Zero;
            for (int i = 0; i < n; i += w) b += new Vector<float>(hs.Slice(i)) * new Vector<float>(tail, i);
            var a = Vector<float>.Zero;
            var hh = hs.Slice(n - head.Length);
            for (int i = 0; i < head.Length; i += w) a += new Vector<float>(hh.Slice(i)) * new Vector<float>(head, i);
            return (Vector.Sum(a), Vector.Sum(b));
        }
    }

    // The DECAY, ATTACK and DYNAMIC switches, tuned on note-onset statistics of the Ibanez
    // recording (decay rate per band, envelope shape around each pick) with the same DI.

    /// <summary>Crossovers of the shaper's four bands. Each band is the residual of the ones below, so the
    /// bands always sum back to the input.</summary>
    static readonly double[] SplitHz = { 400, 1500, 5000 };
    /// <summary>DECAY: the slowest each band may fade after a pick, dB/s (a real top soaks up the string's
    /// energy, trebles fastest).</summary>
    static readonly double[] DecayDbS = { 20, 17, 22, 34 };
    /// <summary>ATTACK: per band, gain at the pick (dB), fading out over <see cref="AttackMs"/>: the pickup's
    /// upper-mid pick spike tamed and the string zing raised.</summary>
    static readonly double[] AttackDb = { 0, 0, -2, 3 };
    const double AttackMs = 20;
    /// <summary>DYNAMIC: treble-shelf dB per dB that a pick is harder/softer than the recent picks, the
    /// shelf's limit, and an offset that keeps the average tone where it was matched (harder picks are also
    /// louder, so they would otherwise brighten the overall sound).</summary>
    const double DynDbPerDb = 0.5, DynMaxDb = 6, DynOffsetDb = 1;

    static double Coef(double fs, double ms) => 1 - Math.Exp(-1000 / (fs * ms));

    /// <summary>
    /// The acoustic channel's three switches, applied to the pickup-corrected string signal before the body.
    /// DECAY caps each band's envelope so it can't fade slower than an acoustic's; a pick detector drives
    /// ATTACK (per-band gains that fade out after each pick) and DYNAMIC (a treble shelf set at each pick by
    /// how hard it was picked compared with the recent picks).
    /// </summary>
    sealed class Shaper
    {
        readonly double fs;
        readonly Biquad[] split = new Biquad[3];
        readonly double[] env = new double[4], slow = new double[4], gain = { 1, 1, 1, 1 };
        /// <summary>DECAY: the envelope a band may not exceed.</summary>
        readonly double[] cap = new double[4];
        readonly double envA, slowA;
        readonly double[] envR, gainA, decayK;
        /// <summary>Pick detector: fast and slow broadband envelopes, and the hold-off between picks.</summary>
        double pickFast, pickSlow;
        readonly double pickFa, pickFr, pickSa;
        int hold;
        /// <summary>ATTACK: 1 at a pick, decaying.</summary>
        double since;
        readonly double sinceK;
        /// <summary>DYNAMIC: the pick's peak so far, samples until it is judged, the running average of pick
        /// peaks (dB), and the shelf gain (target and smoothed).</summary>
        double velPeak, velAvgDb = double.NaN, shelfDb, shelfS;
        int velWait, tick;
        readonly double shelfA;
        Biquad shelf;

        public Shaper(double fs)
        {
            this.fs = fs;
            for (int i = 0; i < 3; i++) split[i].LowPass(fs, SplitHz[i], 0.7071);
            shelf.HighShelf(fs, 2500, 0);
            envA = Coef(fs, 1);
            envR = new[] { Coef(fs, 40), Coef(fs, 20), Coef(fs, 10), Coef(fs, 10) };
            slowA = Coef(fs, 30);
            gainA = new[] { Coef(fs, 8), Coef(fs, 4), Coef(fs, 2), Coef(fs, 1) };
            decayK = Array.ConvertAll(DecayDbS, r => Db(-r / fs));
            pickFa = Coef(fs, 0.5);
            pickFr = Coef(fs, 50);
            pickSa = Coef(fs, 40);
            sinceK = Math.Exp(-1000 / (fs * AttackMs));
            shelfA = Coef(fs, 20);
        }

        public double Process(double x, bool decay, bool attack, bool dynamic)
        {
            // pick detector: the fast envelope jumping 5 dB over the slow one, above -50 dBFS so the noise
            // floor between phrases doesn't count
            double ax = Math.Abs(x);
            pickFast += (ax > pickFast ? pickFa : pickFr) * (ax - pickFast);
            pickSlow += pickSa * (pickFast - pickSlow);
            bool pick = hold == 0 && pickFast > 0.003 && pickFast > 1.8 * pickSlow;
            if (pick)
            {
                hold = (int)(fs * 0.03);
                since = 1;
                velPeak = 0;
                velWait = (int)(fs * 0.005);
            }
            else hold = Math.Max(hold - 1, 0);
            since *= sinceK;
            // DYNAMIC judges each pick 5 ms in, against the running average of recent picks
            if (velWait > 0)
            {
                velPeak = Math.Max(velPeak, pickFast);
                velWait--;
                if (velWait == 0)
                {
                    double peakDb = 20 * Math.Log10(velPeak + 1e-7);
                    if (double.IsNaN(velAvgDb)) velAvgDb = peakDb;
                    double rel = peakDb - velAvgDb;
                    shelfDb = Math.Clamp(DynDbPerDb * rel - DynOffsetDb, -DynMaxDb, DynMaxDb);
                    // catch up quickly with harder playing, forget it slowly
                    velAvgDb += (rel > 0 ? 0.5 : 0.15) * rel;
                }
            }

            double y = x;
            if (decay || attack)
            {
                Span<double> b = stackalloc double[4];
                double r = x;
                for (int i = 0; i < 3; i++) { b[i] = split[i].Process(r); r -= b[i]; }
                b[3] = r;
                y = 0;
                for (int i = 0; i < 4; i++)
                {
                    double a = Math.Abs(b[i]);
                    env[i] += (a > env[i] ? envA : envR[i]) * (a - env[i]);
                    slow[i] += slowA * (env[i] - slow[i]);
                    double e = env[i] + 1e-9;
                    double g = 1;
                    if (decay)
                    {
                        // the cap follows the band up whenever it jumps 3 dB over its recent level (new energy
                        // from a pick in this band; a pick elsewhere doesn't refresh it), then falls at the
                        // band's acoustic decay rate
                        if (e > 1.4 * slow[i]) cap[i] = Math.Max(cap[i], e);
                        else cap[i] *= decayK[i];
                        if (e > cap[i]) g = Math.Max(cap[i] / e, 0.01);
                    }
                    if (attack) g *= Db(AttackDb[i] * since);
                    gain[i] += gainA[i] * (g - gain[i]);
                    y += gain[i] * b[i];
                }
            }
            if (dynamic)
            {
                shelfS += shelfA * (shelfDb - shelfS);
                if (++tick >= 32)
                {
                    tick = 0;
                    shelf.HighShelf(fs, 2500, shelfS);
                }
                y = shelf.Process(y);
            }
            return y;
        }
    }

    /// <summary>Crossovers of the stereo pair's side bands.</summary>
    static readonly double[] PairSplitHz = { 400, 3000 };
    /// <summary>Side level per band relative to the mid, set so the Ibanez preset's L/R coherence follows the
    /// Ibanez recording's (0.95 at 100-250 Hz, 0.83, 0.59, 0.31, 0.1 at 4-10 kHz).</summary>
    static readonly double[] PairSide = { 0.03, 0.2, 0.75 };
    /// <summary>Each mic's reflection pattern: length, number of reflections, and decay time constant.</summary>
    const double PairMs = 400, PairDecayMs = 150;
    const int PairTaps = 400;

    /// <summary>
    /// A spaced stereo pair in a room: both mics hear the guitar alike, but each hears its own pattern of
    /// reflections. The difference between them (the side) is the mid run through a sparse random-sign reflection
    /// pattern ("velvet noise"), weighted per band: long wavelengths reach both mics alike, short ones don't.
    /// Left = mid + side, right = mid - side, so the mono sum is the mid alone.
    /// </summary>
    sealed class Pair
    {
        readonly Biquad[] split = new Biquad[2];
        /// <summary>Reflection delays (samples) and gains.</summary>
        readonly int[] tapD = new int[PairTaps];
        readonly double[] tapG = new double[PairTaps];
        /// <summary>History written twice, so a delay never wraps.</summary>
        readonly double[] hist;
        int pos;

        public Pair(double fs)
        {
            for (int i = 0; i < 2; i++) split[i].LowPass(fs, PairSplitHz[i], 0.7071);
            int len = (int)(fs * PairMs / 1000);
            var rng = new Rng(0x7C3E51A9);
            double energy = 0;
            for (int k = 0; k < PairTaps; k++)
            {
                // one reflection per segment, after the first millisecond, decaying like a small room
                double seg = (double)len / PairTaps;
                int d = (int)(fs * 0.001) + (int)((k + rng.Next()) * seg);
                double sign = rng.Next() < 0.5 ? 1 : -1;
                double g = sign * Math.Exp(-(double)d / (fs * PairDecayMs / 1000));
                tapD[k] = d;
                tapG[k] = g;
                energy += g * g;
            }
            for (int k = 0; k < PairTaps; k++) tapG[k] /= Math.Sqrt(energy);
            hist = new double[2 * (len + (int)(fs * 0.001) + 2)];
        }

        public double Process(double mid)
        {
            double lo = split[0].Process(mid);
            double rest = mid - lo;
            double md = split[1].Process(rest);
            double x = PairSide[0] * lo + PairSide[1] * md + PairSide[2] * (rest - md);
            int n = hist.Length / 2;
            pos = (pos + 1) % n;
            hist[pos] = x;
            hist[pos + n] = x;
            int now = pos + n;
            double y = 0;
            for (int k = 0; k < PairTaps; k++) y += tapG[k] * hist[now - tapD[k]];
            return y;
        }
    }

    readonly double fs;
    readonly BodyFir fir;
    readonly Shaper shaper;
    /// <summary>The stereo pair, and its side output for the block.</summary>
    readonly Pair pair;
    float[] side = new float[0];
    /// <summary>Pickup correction: rumble high-pass, coil-resonance cut, top shelf, band limit.</summary>
    readonly Biquad[] pick = new Biquad[4], sparkIn = new Biquad[2], sparkOut = new Biquad[2], res = new Biquad[3], eq = new Biquad[4];
    double sparkEnv;
    readonly double sparkA, smooth;
    readonly float[] last = { -1, -1, -1, -1 };
    bool lastBright;
    double sBody = -1, sLevel, sReso, sAir;

    /// <summary>Builds the body IR and filters; call off the audio thread.</summary>
    public AcousticSim(double fs)
    {
        this.fs = fs;
        pick[0].HighPass(fs, 70, 0.7);
        pick[1].Peak(fs, 2540, 4.0, -15);
        pick[2].HighShelf(fs, 2920, 15);
        pick[3].LowPass(fs, Math.Min(17000, 0.45 * fs), 0.7);
        for (int i = 0; i < 2; i++) { sparkIn[i].HighPass(fs, 2500, 0.7); sparkOut[i].HighPass(fs, 5000, 0.7); }
        // air cavity (Helmholtz) and the first two top-plate modes
        res[0].BandPass(fs, 98, 5);
        res[1].BandPass(fs, 196, 7);
        res[2].BandPass(fs, 232, 8);
        fir = new BodyFir(BodyIr(fs), fs);
        shaper = new Shaper(fs);
        pair = new Pair(fs);
        sparkA = 1 - Math.Exp(-1 / (fs * 0.01));
        smooth = 1 - Math.Exp(-1 / (fs * 0.02));
    }

    void Update(AmpParams p)
    {
        float bass = p.Bass.Value, mid = p.Middle.Value, treble = p.Treble.Value, pres = p.Presence.Value;
        if (bass == last[0] && mid == last[1] && treble == last[2] && pres == last[3] && p.Bright == lastBright) return;
        last[0] = bass; last[1] = mid; last[2] = treble; last[3] = pres; lastBright = p.Bright;
        eq[0].LowShelf(fs, 120, -9 + 18 * bass);
        eq[1].Peak(fs, 700, 0.8, -8 + 16 * mid);
        eq[2].HighShelf(fs, 4000, -10 + 20 * treble);
        eq[3].HighShelf(fs, 9000, -4 + 10 * pres + (p.Bright ? 4 : 0));
    }

    /// <summary>The stereo pair's side signal for the last block: left = mid + side, right = mid - side.</summary>
    public ReadOnlySpan<float> Side(int n) => side.AsSpan(0, n);

    /// <summary>Processes a mono block in place (the pair's mid; the side goes to <see cref="Side"/>).</summary>
    public void Process(Span<float> buf, AmpParams p)
    {
        if (side.Length < buf.Length) side = new float[buf.Length];
        Update(p);
        double tBody = p.Gain.Value;
        double tLevel = Db(-20 + 30 * p.Master.Value) * Makeup;
        double tReso = 2 * p.Depth.Value * p.Depth.Value;
        double tAir = 0.5 * p.Presence.Value;
        bool decay = p.Decay, attack = p.Attack, dynamic = p.Dynamic;
        // first block: start at the targets instead of fading in
        if (sBody < 0) { sBody = tBody; sLevel = tLevel; sReso = tReso; sAir = tAir; }
        for (int n = 0; n < buf.Length; n++)
        {
            sBody += smooth * (tBody - sBody);
            sLevel += smooth * (tLevel - sLevel);
            sReso += smooth * (tReso - sReso);
            sAir += smooth * (tAir - sAir);

            double x = buf[n];
            for (int i = 0; i < pick.Length; i++) x = pick[i].Process(x);
            // sparkle: a level-independent 2nd-harmonic generator on the top octaves,
            // for the string zing a magnetic pickup doesn't hear
            double e = sparkIn[1].Process(sparkIn[0].Process(x));
            sparkEnv += sparkA * (Math.Abs(e) - sparkEnv);
            double d = e * e / (sparkEnv + 1e-5);
            x += sAir * sparkOut[1].Process(sparkOut[0].Process(d));
            if (decay || attack || dynamic) x = shaper.Process(x, decay, attack, dynamic);

            var (head, tail) = fir.Process((float)x);
            double y = head + sBody * tail;
            y += sReso * (res[0].Process(y) + 0.7 * res[1].Process(y) + 0.4 * res[2].Process(y));
            for (int i = 0; i < eq.Length; i++) y = eq[i].Process(y);
            buf[n] = (float)(y * sLevel);
            side[n] = (float)(pair.Process(y) * sLevel);
        }
    }
}
