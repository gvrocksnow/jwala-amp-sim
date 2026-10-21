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
        foreach (var e in AllEffects)
        {
            e.Enabled = Get(e.Id + ".ON", 0) > 0.5f;
            foreach (var p in e.Params) p.Value = Get(e.Id + "." + p.Name, p.Default);
        }
    }
}

/// <summary>The running signal chain at one sample rate: gate → stomps → amp → cab → stereo FX.</summary>
public sealed class Rig
{
    public readonly double SampleRate;
    public readonly RigModel M;
    public float InputPeak, OutputPeakL, OutputPeakR, GateOpen;

    readonly AmpEngine amp;
    readonly Convolver cab = new();
    int cabModel = -1; float[] cabIr;
    Biquad micShelf, micLp, proximity, lowCut, highCut;
    readonly DelayLine room;
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
        room = new DelayLine((int)(fs * 0.02));
        inHp.SetLowPass(fs, 25); outDcL.SetLowPass(fs, 10); outDcR.SetLowPass(fs, 10);
        gateDet = 1 - Math.Exp(-1 / (fs * 0.04));
        gateAtk = 1 - Math.Exp(-1 / (fs * 0.0008));
        gateRel = 1 - Math.Exp(-1 / (fs * 0.07));
        smooth = 1 - Math.Exp(-1 / (fs * 0.02));
        UpdateCabIr();
    }

    static double Db(double db) => Math.Pow(10, db / 20);

    /// <summary>Rebuilds the cab IR. Call from the UI thread when the model or user IR changes.</summary>
    public void UpdateCabIr()
    {
        var c = M.Cab;
        float[] ir;
        if (c.Model == 3 && c.IrData != null)
        {
            ir = Resample(c.IrData, c.IrRate, (int)SampleRate);
            ir = (float[])ir.Clone();
            Convolver.Normalize(ir, SampleRate);
        }
        else ir = Convolver.SyntheticCab(SampleRate, Math.Clamp(c.Model, 0, 2));
        cab.SetImpulse(ir);
        cabModel = c.Model; cabIr = c.IrData;
    }

    public bool CabStale => cabModel != M.Cab.Model || cabIr != M.Cab.IrData;

    static float[] Resample(float[] x, int from, int to)
    {
        if (from == to) return x;
        int n = (int)((long)x.Length * to / from);
        var y = new float[n];
        double step = (double)from / to;
        for (int i = 0; i < n; i++)
        {
            double t = i * step; int j = (int)t; double f = t - j;
            float a = x[Math.Min(j, x.Length - 1)], b = x[Math.Min(j + 1, x.Length - 1)];
            y[i] = (float)(a + (b - a) * f);
        }
        return y;
    }

    void UpdateMic()
    {
        var c = M.Cab; double fs = SampleRate;
        if (c.MicPos.Value != lpPos)
        {
            lpPos = c.MicPos.Value;
            micShelf.HighShelf(fs, 2500, 2.5 - 10 * lpPos);   // cap = bright, cone = dark
            micLp.LowPass(fs, 16000 - 9000 * lpPos, 0.7);
        }
        if (c.Distance.Value != lpDist)
        {
            lpDist = c.Distance.Value;
            proximity.LowShelf(fs, 180, 3.5 - 8 * lpDist);     // proximity effect fades with distance
        }
        if (c.LowCut.Value != lpLow) { lpLow = c.LowCut.Value; lowCut.HighPass(fs, 20 * Math.Pow(15, lpLow), 0.707); }
        if (c.HighCut.Value != lpHigh) { lpHigh = c.HighCut.Value; highCut.LowPass(fs, Math.Min(0.45 * fs, 3000 * Math.Pow(20000 / 3000.0, lpHigh)), 0.707); }
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

        if (M.Cab.Enabled)
        {
            // an acoustic guitar isn't played through a speaker: keep only the mic and room
            if (M.Amp.Channel != Channel.Acoustic) cab.Process(buf);
            UpdateMic();
            double roomMix = M.Cab.Distance.Value * 0.3, fs = SampleRate;
            for (int i = 0; i < n; i++)
            {
                double x = buf[i];
                room.Write(x);
                x += roomMix * (room.Read(fs * 0.0031) * 0.6 - room.Read(fs * 0.0057) * 0.45 + room.Read(fs * 0.0089) * 0.3);
                x = micLp.Process(micShelf.Process(proximity.Process(x)));
                buf[i] = (float)highCut.Process(lowCut.Process(x));
            }
        }

        buf.CopyTo(l); buf.CopyTo(r);
        // the acoustic channel is a stereo pair of mics; its side skips the cab's mic and room
        var side = amp.AcousticSide(n);
        for (int i = 0; i < side.Length; i++) { l[i] += side[i]; r[i] -= side[i]; }
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
        ("Glass Clean", S("amp.CHANNEL=0 amp.GAIN=0.45 amp.BASS=0.55 amp.MIDDLE=0.45 amp.TREBLE=0.65 amp.BRIGHT=1 comp.ON=1 comp.SUSTAIN=0.45 chorus.ON=1 chorus.MIX=0.35 delay.ON=1 delay.TIME=0.62 delay.MIX=0.22 reverb.ON=1 reverb.MIX=0.3 cab.MODEL=1")),
        ("Ambient Shimmer", S("amp.CHANNEL=0 amp.GAIN=0.4 amp.TREBLE=0.6 comp.ON=1 delay.ON=1 delay.TIME=0.72 delay.FEEDBACK=0.55 delay.MIX=0.35 delay.MOD=0.45 delay.SPREAD=1 reverb.ON=1 reverb.DECAY=0.85 reverb.SIZE=0.9 reverb.SHIMMER=0.45 reverb.MIX=0.55 reverb.PREDELAY=0.3 cab.MODEL=1")),
        ("Classic Crunch", S("doubler.ON=1 doubler.AMOUNT=0.4 amp.CHANNEL=1 amp.GAIN=0.55 amp.MIDDLE=0.7 amp.TREBLE=0.6 amp.PRESENCE=0.55 reverb.ON=1 reverb.DECAY=0.3 reverb.MIX=0.18 cab.MODEL=0")),
        ("Blues Lead", S("amp.CHANNEL=1 amp.GAIN=0.6 amp.MIDDLE=0.7 od.ON=1 od.DRIVE=0.5 od.LEVEL=0.55 delay.ON=1 delay.TIME=0.5 delay.MIX=0.18 delay.FEEDBACK=0.25 reverb.ON=1 reverb.MIX=0.22 cab.MODEL=2")),
        ("Modern Metal", S("doubler.ON=1 amp.CHANNEL=2 amp.GAIN=0.55 amp.BASS=0.55 amp.MIDDLE=0.45 amp.TREBLE=0.62 amp.DEPTH=0.6 global.GATE=0.45 od.ON=1 od.DRIVE=0 od.LEVEL=0.85 od.TONE=0.6 cab.MODEL=0 cab.MIC POS=0.25")),
        ("Soaring Solo", S("amp.CHANNEL=2 amp.GAIN=0.7 amp.MIDDLE=0.7 amp.PRESENCE=0.55 od.ON=1 od.DRIVE=0.2 od.LEVEL=0.75 delay.ON=1 delay.TIME=0.66 delay.FEEDBACK=0.42 delay.MIX=0.3 delay.DUCK=0.6 delay.SPREAD=0.8 reverb.ON=1 reverb.DECAY=0.55 reverb.MIX=0.28")),
        ("Fuzz Wall", S("amp.CHANNEL=1 amp.GAIN=0.4 fuzz.ON=1 fuzz.SUSTAIN=0.75 fuzz.TONE=0.45 phaser.ON=1 phaser.MIX=0.4 reverb.ON=1 reverb.MIX=0.25 cab.MODEL=0")),
        ("Rodent Grunge", S("doubler.ON=1 amp.CHANNEL=1 amp.GAIN=0.45 dist.ON=1 dist.DIST=0.65 dist.FILTER=0.4 chorus.ON=1 chorus.RATE=0.25 chorus.MIX=0.4 cab.MODEL=0")),
        ("Gazebo Acoustic", S("amp.CHANNEL=3 amp.GAIN=0.56 amp.BASS=0 amp.MIDDLE=0 amp.TREBLE=0.73 amp.PRESENCE=0.45 amp.DEPTH=0 amp.MASTER=0.5 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.2 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.32")),
        ("Simple Steps Acoustic", S("amp.CHANNEL=3 amp.GAIN=0.85 amp.BASS=0.2 amp.MIDDLE=0.87 amp.TREBLE=1 amp.PRESENCE=0.36 amp.DEPTH=0 amp.MASTER=0.5 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.35 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.35")),
        ("Ibanez Acoustic", S("amp.CHANNEL=3 amp.GAIN=0.5 amp.BASS=0.5 amp.MIDDLE=0.5 amp.TREBLE=0.5 amp.PRESENCE=0.4 amp.DEPTH=0.5 amp.MASTER=0.5 amp.DECAY=1 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.35 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.15")),
        ("Aggressive Waters", S("amp.CHANNEL=2 amp.GAIN=0.17 amp.BASS=0.26 amp.MIDDLE=0.92 amp.TREBLE=1 amp.PRESENCE=0 amp.DEPTH=0.44 amp.MASTER=0.64 cab.MODEL=1 cab.MIC POS=0.15 cab.DISTANCE=0 cab.LOW CUT=0.08 cab.HIGH CUT=1 reverb.ON=1 reverb.DECAY=1 reverb.PREDELAY=0.39 reverb.SIZE=0.6 reverb.TONE=1 reverb.MOD=0.15 reverb.MIX=0.15")),
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
