using System;
using System.Collections.Generic;
using System.Globalization;

namespace Jwala.Dsp;

/// <summary>Cabinet settings: model / user IR plus virtual mic placement.</summary>
public sealed class CabParams
{
    public volatile bool Enabled = true;
    public volatile int Model;           // 0..2 built-in, 3 = user IR
    public readonly Param MicPos = new("MIC POS", 0.3f, v => v < 0.15f ? "CAP" : v < 0.6f ? "CAP EDGE" : "CONE");
    public readonly Param Distance = new("DISTANCE", 0.15f, v => (1 + v * 30).ToString("0") + " cm");
    public readonly Param LowCut = new("LOW CUT", 0.15f, v => Fmt.Hz(20 * Math.Pow(15, v)));
    public readonly Param HighCut = new("HIGH CUT", 0.8f, v => v > 0.98f ? "OFF" : Fmt.Hz(3000 * Math.Pow(20000 / 3000.0, v)));
    public string IrPath;
    public float[] IrData; public int IrRate;
    public Param[] Knobs => new[] { MicPos, Distance, LowCut, HighCut };
}

/// <summary>Everything the user can set. Survives audio restarts; presets read/write it.</summary>
public sealed class RigModel
{
    public readonly AmpParams Amp = new();
    public readonly CabParams Cab = new();
    public readonly EqParams Eq = new();
    public readonly Tuner Tuner = new();
    public readonly SampleEffect[] Pre = { new Compressor(), new Overdrive(), new Distortion(), new Fuzz() };
    public readonly StereoEffect[] Post = { new Doubler(), new Chorus(), new Phaser(), new Delay(), new Reverb() };

    public IEnumerable<Effect> AllEffects { get { foreach (var e in Pre) yield return e; foreach (var e in Post) yield return e; } }

    public Dictionary<string, float> Save()
    {
        var d = new Dictionary<string, float>();
        foreach (var p in Amp.Globals) d["global." + p.Name] = p.Value;
        foreach (var p in Amp.HeadKnobs) d["amp." + p.Name] = p.Value;
        d["amp.CHANNEL"] = (int)Amp.Channel; d["amp.BRIGHT"] = Amp.Bright ? 1 : 0;
        d["amp.DECAY"] = Amp.Decay ? 1 : 0; d["amp.ATTACK"] = Amp.Attack ? 1 : 0; d["amp.DYNAMIC"] = Amp.Dynamic ? 1 : 0;
        d["cab.ON"] = Cab.Enabled ? 1 : 0; d["cab.MODEL"] = Cab.Model;
        foreach (var p in Cab.Knobs) d["cab." + p.Name] = p.Value;
        d["eq.ON"] = Eq.Enabled ? 1 : 0;
        foreach (var p in Eq.Knobs) d["eq." + p.Name] = p.Value;
        foreach (var e in AllEffects)
        {
            d[e.Id + ".ON"] = e.Enabled ? 1 : 0;
            foreach (var p in e.Params) d[e.Id + "." + p.Name] = p.Value;
        }
        return d;
    }

    /// <summary>Applies a state; anything missing falls back to defaults (presets can be partial).</summary>
    public void Load(IDictionary<string, float> d, bool includeGlobals = false)
    {
        float Get(string k, float def) => d != null && d.TryGetValue(k, out var v) ? v : def;
        if (includeGlobals) foreach (var p in Amp.Globals) p.Value = Get("global." + p.Name, p.Default);
        foreach (var p in Amp.HeadKnobs) p.Value = Get("amp." + p.Name, p.Default);
        Amp.Channel = (Channel)(int)Get("amp.CHANNEL", 1); Amp.Bright = Get("amp.BRIGHT", 0) > 0.5f;
        Amp.Decay = Get("amp.DECAY", 0) > 0.5f; Amp.Attack = Get("amp.ATTACK", 0) > 0.5f; Amp.Dynamic = Get("amp.DYNAMIC", 0) > 0.5f;
        Cab.Enabled = Get("cab.ON", 1) > 0.5f;
        int m = (int)Get("cab.MODEL", 0); Cab.Model = m == 3 && Cab.IrData == null ? 0 : m;
        foreach (var p in Cab.Knobs) p.Value = Get("cab." + p.Name, p.Default);
        Eq.Enabled = Get("eq.ON", 0) > 0.5f;
        foreach (var p in Eq.Knobs) p.Value = Get("eq." + p.Name, p.Default);
        foreach (var e in AllEffects)
        {
            e.Enabled = Get(e.Id + ".ON", 0) > 0.5f;
            foreach (var p in e.Params) p.Value = Get(e.Id + "." + p.Name, p.Default);
        }
    }
}

/// <summary>The running signal chain at one sample rate: gate → stomps → amp → cab → graphic EQ → stereo FX.</summary>
public sealed class Rig
{
    public readonly double SampleRate;
    public readonly RigModel M;
    public float InputPeak, OutputPeakL, OutputPeakR, GateOpen;

    /// <summary>Early reflections off the room (delay in seconds, gain), slightly different per side.</summary>
    static readonly (double T, double G)[] RoomL = { (0.0029, 0.55), (0.0047, -0.42), (0.0073, 0.33), (0.0109, -0.24), (0.0157, 0.17) };
    static readonly (double T, double G)[] RoomR = { (0.0034, 0.52), (0.0055, -0.40), (0.0082, 0.31), (0.0121, -0.22), (0.0171, 0.15) };

    /// <summary>Schroeder allpass: smears the room's reflections so they don't ring.</summary>
    sealed class Allpass
    {
        readonly double[] buf; int i; readonly double g;
        public Allpass(double len, double g) { buf = new double[Math.Max((int)len, 1)]; this.g = g; }
        public double Process(double x)
        {
            double d = buf[i], w = x + g * d;
            buf[i] = w;
            i = (i + 1) % buf.Length;
            return d - g * w;
        }
    }

    readonly AmpEngine amp;
    readonly Convolver cab = new();
    readonly GraphicEq eq;
    int cabModel = -1; float[] cabIr;
    // both sides share coefficients; each keeps its own state
    readonly Biquad[] micShelf = new Biquad[2], micLp = new Biquad[2], proximity = new Biquad[2], lowCut = new Biquad[2], highCut = new Biquad[2];
    readonly DelayLine room;
    readonly OnePole[] roomLp = new OnePole[2];
    readonly Allpass[][] roomAp;
    float lpPos = -1, lpDist = -1, lpLow = -1, lpHigh = -1;
    OnePole inHp, outDcL, outDcR;
    double gateEnv, gateGain, gateDet, gateAtk, gateRel, sIn, sOut, smooth;
    float[] tmp = new float[0];

    public Rig(double fs, RigModel m)
    {
        SampleRate = fs; M = m;
        foreach (var e in m.Pre) if (e.Slot == EffectSlot.Mono) e.Prepare(fs);
        var os = Array.FindAll(m.Pre, e => e.Slot == EffectSlot.Oversampled);
        amp = new AmpEngine(fs, m.Amp, os);
        foreach (var e in m.Post) e.Prepare(fs);
        eq = new GraphicEq(fs, m.Eq);
        room = new DelayLine((int)(fs * 0.02));
        for (int c = 0; c < 2; c++) roomLp[c].SetLowPass(fs, 5500); // the walls soak up the top
        roomAp = new[]
        {
            new[] { new Allpass(fs * 0.0013, 0.5), new Allpass(fs * 0.0023, 0.5) },
            new[] { new Allpass(fs * 0.0017, 0.5), new Allpass(fs * 0.0029, 0.5) },
        };
        inHp.SetLowPass(fs, 25); outDcL.SetLowPass(fs, 10); outDcR.SetLowPass(fs, 10);
        gateDet = 1 - Math.Exp(-1 / (fs * 0.04));
        gateAtk = 1 - Math.Exp(-1 / (fs * 0.0008));
        gateRel = 1 - Math.Exp(-1 / (fs * 0.07));
        smooth = 1 - Math.Exp(-1 / (fs * 0.02));
        UpdateCabIr();
    }

    static double Db(double db) => Math.Pow(10, db / 20);

    /// <summary>Rebuilds the cab IRs. Call from the UI thread when the model or user IR changes.</summary>
    public void UpdateCabIr()
    {
        var c = M.Cab;
        int model = c.Model; float[] data = c.IrData; int rate = c.IrRate;
        cab.Swap(Cab.Build(SampleRate, model, data, rate));
        cabModel = model; cabIr = data;
    }

    public bool CabStale => cabModel != M.Cab.Model || cabIr != M.Cab.IrData;

    static void Set(Biquad[] f, Action<Biquad[]> design) { design(f); f[1].CopyCoefs(f[0]); }

    void UpdateMic()
    {
        var c = M.Cab; double fs = SampleRate;
        cab.SetMic(c.MicPos.Value);
        if (c.MicPos.Value != lpPos)
        {
            lpPos = c.MicPos.Value;
            // only for user IRs and the acoustic channel: the built-in cabs are designed per position
            double p = lpPos;
            Set(micShelf, f => f[0].HighShelf(fs, 2500, 2.5 - 10 * p));   // cap = bright, cone = dark
            Set(micLp, f => f[0].LowPass(fs, 16000 - 9000 * p, 0.7));
        }
        if (c.Distance.Value != lpDist)
        {
            lpDist = c.Distance.Value;
            double d = lpDist;
            Set(proximity, f => f[0].LowShelf(fs, 180, 3.5 - 8 * d));     // proximity effect fades with distance
        }
        if (c.LowCut.Value != lpLow) { lpLow = c.LowCut.Value; double v = lpLow; Set(lowCut, f => f[0].HighPass(fs, 20 * Math.Pow(15, v), 0.707)); }
        if (c.HighCut.Value != lpHigh) { lpHigh = c.HighCut.Value; double v = lpHigh; Set(highCut, f => f[0].LowPass(fs, Math.Min(0.45 * fs, 3000 * Math.Pow(20000 / 3000.0, v)), 0.707)); }
    }

    double Reflections((double T, double G)[] taps)
    {
        double fs = SampleRate, e = 0;
        foreach (var (t, g) in taps) e += g * room.Read(fs * t);
        return e;
    }

    public void Process(ReadOnlySpan<float> input, Span<float> l, Span<float> r)
    {
        int n = input.Length;
        if (tmp.Length < n) tmp = new float[n];
        var buf = tmp.AsSpan(0, n);
        var a = M.Amp;
        double tIn = Db(-12 + 24 * a.Input.Value);
        double tOut = a.Output.Value < 0.001 || (M.Tuner.Active && M.Tuner.MuteOutput) ? 0 : Db(-40 + 46 * a.Output.Value);
        M.Tuner.Write(input);
        double gateThr = a.Gate.Value < 0.01 ? 0 : Db(-80 + 50 * a.Gate.Value);

        // input gain, DC/rumble block, noise gate, base-rate pedals (compressor)
        float inPk = 0;
        foreach (var e in M.Pre) if (e.Slot == EffectSlot.Mono) e.Update();
        for (int i = 0; i < n; i++)
        {
            sIn += smooth * (tIn - sIn);
            double x = inHp.HighPass(input[i] * sIn);
            float ax = (float)Math.Abs(x); if (ax > inPk) inPk = ax;
            gateEnv += gateDet * (Math.Abs(x) - gateEnv);
            double gt = gateThr == 0 ? 1 : gateEnv > gateThr ? 1 : gateEnv < gateThr * 0.5 ? 0 : (gateGain > 0.5 ? 1 : 0);
            gateGain += (gt > gateGain ? gateAtk : gateRel) * (gt - gateGain);
            x *= gateGain;
            foreach (var e in M.Pre) if (e.Slot == EffectSlot.Mono) x = e.Run(x);
            buf[i] = (float)x;
        }

        amp.Process(buf);

        // an acoustic guitar isn't played through a speaker: keep only the mic and room
        bool acoustic = M.Amp.Channel == Channel.Acoustic;
        bool cabOn = M.Cab.Enabled;
        if (cabOn) UpdateMic();
        if (cabOn && !acoustic) cab.Process(buf, l, r);
        else { buf.CopyTo(l); buf.CopyTo(r); }
        if (cabOn)
        {
            bool micEq = acoustic || !cab.Positional;
            double roomMix = M.Cab.Distance.Value * 0.3;
            for (int i = 0; i < n; i++)
            {
                double xl = l[i], xr = r[i];
                room.Write(0.5 * (xl + xr));
                double el = Reflections(RoomL), er = Reflections(RoomR);
                for (int c = 0; c < 2; c++)
                {
                    double x = c == 0 ? xl : xr;
                    double e = roomAp[c][1].Process(roomAp[c][0].Process(roomLp[c].LowPass(c == 0 ? el : er)));
                    double y = proximity[c].Process(x + roomMix * e);
                    if (micEq) y = micLp[c].Process(micShelf[c].Process(y));
                    float o = (float)highCut[c].Process(lowCut[c].Process(y));
                    if (c == 0) l[i] = o; else r[i] = o;
                }
            }
        }
        // the acoustic channel is a stereo pair of mics; its side skips the cab's mic and room
        var side = amp.AcousticSide(n);
        for (int i = 0; i < side.Length; i++) { l[i] += side[i]; r[i] -= side[i]; }
        eq.Process(l, r);
        foreach (var e in M.Post) e.Process(l, r);

        float pl = 0, pr = 0;
        for (int i = 0; i < n; i++)
        {
            sOut += smooth * (tOut - sOut);
            double ol = Clip(outDcL.HighPass(l[i]) * sOut), or = Clip(outDcR.HighPass(r[i]) * sOut);
            l[i] = (float)ol; r[i] = (float)or;
            if (Math.Abs(l[i]) > pl) pl = Math.Abs(l[i]);
            if (Math.Abs(r[i]) > pr) pr = Math.Abs(r[i]);
        }
        InputPeak = Math.Max(inPk, InputPeak * 0.85f);
        OutputPeakL = Math.Max(pl, OutputPeakL * 0.85f);
        OutputPeakR = Math.Max(pr, OutputPeakR * 0.85f);
        GateOpen = (float)gateGain;
    }

    /// <summary>Transparent below 0.8, smooth limiting above, never exceeds ±1.</summary>
    static double Clip(double x)
    {
        if (!double.IsFinite(x)) return 0;
        double ax = Math.Abs(x);
        if (ax <= 0.8) return x;
        return Math.Sign(x) * (0.8 + 0.2 * Math.Tanh((ax - 0.8) / 0.2));
    }
}

/// <summary>Factory presets (partial states; unspecified values use defaults, effects off).</summary>
public static class FactoryPresets
{
    public static readonly (string Name, Dictionary<string, float> State)[] All =
    {
        ("Init", new()),
        ("Glass Clean", S("amp.BASS=0.55 amp.BRIGHT=1 amp.CHANNEL=0 amp.GAIN=0.45 amp.MIDDLE=0.45 amp.TREBLE=0.65 cab.MODEL=1 chorus.MIX=0.35 chorus.ON=1 comp.ON=1 comp.SUSTAIN=0.45 delay.MIX=0.22 delay.ON=1 delay.TIME=0.62 reverb.ON=1 reverb.SHIMMER=0.595")),
        ("Ambient Shimmer", S("amp.BASS=0.762 amp.CHANNEL=2 amp.DEPTH=0.375 amp.GAIN=0.23 amp.MASTER=0.805 amp.MIDDLE=0.25 amp.PRESENCE=0.243 amp.TREBLE=0.713 cab.MIC POS=0.731 cab.MODEL=1 comp.ON=1 delay.FEEDBACK=0.55 delay.MIX=0.35 delay.MOD=0.45 delay.SPREAD=1 delay.TIME=0.754 od.ON=1 reverb.DECAY=0.51 reverb.MIX=0.55 reverb.ON=1 reverb.PREDELAY=0.3 reverb.SHIMMER=0.45 reverb.SIZE=0.9")),
        ("Classic Crunch", S("amp.GAIN=0.55 amp.MIDDLE=0.7 amp.PRESENCE=0.55 doubler.AMOUNT=0.4 doubler.ON=1 reverb.DECAY=0.3 reverb.MIX=0.18 reverb.ON=1")),
        ("Blues Lead", S("amp.MIDDLE=0.7 cab.MODEL=2 delay.FEEDBACK=0.25 delay.MIX=0.18 delay.ON=1 delay.TIME=0.5 od.DRIVE=0.5 od.LEVEL=0.55 od.ON=1 reverb.MIX=0.22 reverb.ON=1")),
        ("Lead II Rhythm", S("amp.BASS=0.55 amp.CHANNEL=4 amp.DEPTH=0.6 amp.GAIN=0.5 amp.MIDDLE=0.45 amp.PRESENCE=0.55 cab.MIC POS=0.25 doubler.ON=1 od.DRIVE=0 od.LEVEL=0.85 od.ON=1 od.TONE=0.6")),
        ("Lead II Solo", S("amp.CHANNEL=4 amp.GAIN=0.65 amp.MIDDLE=0.65 delay.DUCK=0.6 delay.FEEDBACK=0.42 delay.ON=1 delay.SPREAD=0.8 delay.TIME=0.66 od.DRIVE=0.2 od.LEVEL=0.75 od.ON=1 reverb.DECAY=0.55 reverb.MIX=0.28 reverb.ON=1")),
        ("Aggro Lush", S("amp.BASS=0.432 amp.BRIGHT=1 amp.CHANNEL=4 amp.DEPTH=0.853 amp.GAIN=0.275 amp.MASTER=0.873 amp.MIDDLE=0.532 amp.PRESENCE=1 amp.TREBLE=1 cab.DISTANCE=0.12 cab.HIGH CUT=1 cab.LOW CUT=0 cab.MIC POS=0.24 delay.ON=1 eq.125=0.433 eq.16K=0.512 eq.1K=0.568 eq.250=0.458 eq.2K=0.596 eq.4K=0.604 eq.500=0.489 eq.63=0.447 eq.8K=0.578 eq.ON=1 reverb.DECAY=0.62 reverb.MIX=0.663 reverb.ON=1 reverb.PREDELAY=0.34 reverb.SHIMMER=1 reverb.TONE=0.7")),
        ("Fuzz Wall", S("amp.GAIN=0.4 fuzz.ON=1 fuzz.SUSTAIN=0.75 fuzz.TONE=0.45 phaser.MIX=0.4 phaser.ON=1 reverb.MIX=0.25 reverb.ON=1")),
        ("Rodent Grunge", S("amp.GAIN=0.45 chorus.MIX=0.4 chorus.ON=1 chorus.RATE=0.25 dist.DIST=0.65 dist.FILTER=0.4 dist.ON=1 doubler.ON=1")),
        ("Acoustic 1", S("amp.BASS=0 amp.CHANNEL=3 amp.DEPTH=0 amp.GAIN=0.56 amp.MIDDLE=0 amp.PRESENCE=0.45 amp.TREBLE=0.73 cab.ON=0 comp.ATTACK=0.6 comp.LEVEL=0.35 comp.ON=1 comp.SUSTAIN=0.25 reverb.DECAY=0.2 reverb.MIX=0.32 reverb.MOD=0.2 reverb.ON=1 reverb.PREDELAY=0.05 reverb.SIZE=0.5")),
        ("Acoustic 2", S("amp.BASS=0.2 amp.CHANNEL=3 amp.DEPTH=0 amp.GAIN=0.85 amp.MIDDLE=0.87 amp.PRESENCE=0.36 amp.TREBLE=1 cab.ON=0 comp.ATTACK=0.6 comp.LEVEL=0.35 comp.ON=1 comp.SUSTAIN=0.25 reverb.DECAY=0.35 reverb.MIX=0.35 reverb.MOD=0.2 reverb.ON=1 reverb.PREDELAY=0.05 reverb.SIZE=0.5")),
        ("Acoustic 3", S("amp.ATTACK=1 amp.CHANNEL=3 amp.DECAY=1 amp.DEPTH=0.5 amp.DYNAMIC=1 amp.GAIN=0.5 amp.MIDDLE=0.5 amp.PRESENCE=0.4 amp.TREBLE=0.5 cab.ON=0 comp.ATTACK=0.6 comp.LEVEL=0.35 comp.ON=1 comp.SUSTAIN=0.25 reverb.DECAY=0.35 reverb.MIX=0.15 reverb.MOD=0.2 reverb.ON=1 reverb.PREDELAY=0.05 reverb.SIZE=0.5")),
        ("Acoustic Lake", S("amp.BASS=0.625 amp.BRIGHT=1 amp.CHANNEL=3 amp.DEPTH=0.8 amp.GAIN=0.763 amp.MASTER=0.458 amp.MIDDLE=0.537 amp.PRESENCE=0.678 amp.TREBLE=0.657 cab.DISTANCE=0.12 cab.HIGH CUT=1 cab.LOW CUT=0 cab.MIC POS=0.24 delay.ON=1 eq.125=0.836 eq.16K=0.794 eq.1K=0.164 eq.250=0.794 eq.2K=0.193 eq.4K=0.296 eq.500=0.675 eq.63=0.754 eq.8K=0.689 reverb.DECAY=0.62 reverb.MIX=0.663 reverb.ON=1 reverb.PREDELAY=0.34 reverb.SHIMMER=1 reverb.TONE=0.7")),
    };

    static Dictionary<string, float> S(string spec)
    {
        var d = new Dictionary<string, float>();
        // keys may contain spaces ("cab.MIC POS"), so split on '=' boundaries
        var parts = spec.Split('=');
        string key = parts[0];
        for (int i = 1; i < parts.Length; i++)
        {
            string seg = parts[i];
            int sp = i < parts.Length - 1 ? seg.IndexOf(' ') : -1;
            string val = sp < 0 ? seg : seg[..sp];
            d[key.Trim()] = float.Parse(val, CultureInfo.InvariantCulture);
            if (sp >= 0) key = seg[(sp + 1)..];
        }
        return d;
    }
}
