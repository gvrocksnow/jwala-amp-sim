using System;

namespace Jwala.Dsp;

public enum Channel { Clean = 0, Crunch = 1, Lead = 2, Acoustic = 3 }

/// <summary>Amp head knobs plus the global input/gate/output controls.</summary>
public sealed class AmpParams
{
    public readonly Param Input = new("INPUT", 0.5f, v => ((-12 + 24 * v) >= 0 ? "+" : "") + (-12 + 24 * v).ToString("0.0") + " dB");
    public readonly Param Gate = new("GATE", 0.25f, v => v < 0.01f ? "OFF" : (-80 + 50 * v).ToString("0") + " dB");
    public readonly Param Output = new("OUTPUT", 0.6f, v => (-40 + 46 * v).ToString("0.0") + " dB");
    public readonly Param Gain = new("GAIN", 0.6f), Bass = new("BASS", 0.5f), Middle = new("MIDDLE", 0.6f), Treble = new("TREBLE", 0.6f),
        Presence = new("PRESENCE", 0.5f), Depth = new("DEPTH", 0.4f), Master = new("MASTER", 0.5f);
    public volatile Channel Channel = Channel.Crunch;
    public volatile bool Bright;
    /// <summary>Acoustic-channel switches (see Dsp/Acoustic.cs).</summary>
    public volatile bool Decay, Attack, Dynamic;

    public Param[] HeadKnobs => new[] { Gain, Bass, Middle, Treble, Presence, Depth, Master };
    public Param[] Globals => new[] { Input, Gate, Output };
}

/// <summary>
/// One triode gain stage: coupling-cap high-pass, Miller-cap band-limiting, asymmetric
/// grid-conduction clipping with dynamic bias shift (the "blocking" that makes hard
/// picking bloom), plate low-pass, phase inversion.
/// </summary>
sealed class TriodeStage
{
    public double Drive = 1, BiasShift = 0.5;
    OnePole couple, miller, plate, dc;
    double env, atk, rel;
    const double Bias0 = 0.18; // static operating point offset -> even harmonics
    static readonly double Shape0 = Shape(Bias0);

    public void Configure(double fs, double hpHz, double lpHz)
    {
        couple.SetLowPass(fs, hpHz);
        miller.SetLowPass(fs, lpHz);
        plate.SetLowPass(fs, 18000);
        dc.SetLowPass(fs, 8);
        atk = 1 - Math.Exp(-1 / (fs * 0.004));
        rel = 1 - Math.Exp(-1 / (fs * 0.12));
    }

    // Soft, compressive grid-conduction side; harder cutoff side with more headroom.
    static double Shape(double v) => v >= 0 ? v / (1 + v) : -1.6 * Math.Tanh(-v / 1.6);

    public double Process(double x)
    {
        double v = miller.LowPass(couple.HighPass(x)) * Drive;
        double pos = v > 0 ? v : 0;
        env += (pos > env ? atk : rel) * (pos - env);
        double y = Shape(v + Bias0 - env * BiasShift) - Shape0;
        y = plate.LowPass(y);
        y = dc.HighPass(y);
        return -y;
    }
}

/// <summary>
/// Preamp + tone stack + power amp, all at 4x oversampling. Oversampled-slot pedals
/// (drives/fuzz) run inside the same oversampled region so they don't alias either.
/// Channel.Acoustic swaps the tube stages for the acoustic-guitar simulator.
/// </summary>
public sealed class AmpEngine
{
    public readonly double SampleRate;
    readonly AmpParams P;
    readonly SampleEffect[] osPedals;

    readonly double fsOs;
    readonly Oversampler os = new();
    readonly double[] osBuf = new double[Oversampler.Factor];
    readonly TriodeStage[] stages = { new(), new(), new(), new() };
    int stageCount;
    double blend; // Lead only: how much of the first stage's output is blended past the later stages
    readonly ToneStack tone;
    Biquad brightCap, lows, presence, resonance, powerHp;
    double sagEnv, sagAtk, sagRel, sGain, sMaster, smooth;
    double lastPres = -1, lastRes = -1;
    Channel lastCh = (Channel)(-1);
    bool lastBright;
    readonly AcousticSim acoustic;

    public AmpEngine(double sampleRate, AmpParams p, SampleEffect[] osPedals)
    {
        SampleRate = sampleRate; P = p; this.osPedals = osPedals;
        fsOs = sampleRate * Oversampler.Factor;
        foreach (var e in osPedals) e.Prepare(fsOs);
        tone = new ToneStack(fsOs);
        powerHp.HighPass(fsOs, 35, 0.7);
        sagAtk = 1 - Math.Exp(-1 / (fsOs * 0.012));
        sagRel = 1 - Math.Exp(-1 / (fsOs * 0.18));
        smooth = 1 - Math.Exp(-1 / (sampleRate * 0.02));
        acoustic = new AcousticSim(sampleRate);
    }

    static double Db(double db) => Math.Pow(10, db / 20);

    void ConfigureChannel(Channel ch, bool bright)
    {
        // (hp Hz, lp Hz, inter-stage gain, bias shift) per stage.
        // Lead: the bass is cut before the first two stages so low notes don't intermodulate with
        // everything above them (tight, even note to note; a shelf after the stages puts it back),
        // and the Miller corners are low so the stages clip a darker signal: less fizz and hiss,
        // the treble comes from the harmonics instead.
        (double, double, double, double)[] cfg = ch switch
        {
            Channel.Clean => new[] { (25.0, 16000.0, 1.0, 0.25), (40.0, 12000.0, 1.8, 0.3) },
            Channel.Crunch => new[] { (35.0, 14000.0, 1.0, 0.4), (110.0, 10000.0, 5.0, 0.5), (60.0, 8500.0, 2.5, 0.6) },
            _ => new[] { (440.0, 6000.0, 1.0, 0.52), (215.0, 4500.0, 9.0, 0.63), (60.0, 3750.0, 10.0, 0.63), (55.0, 3400.0, 2.5, 0.63) },
        };
        // a little of the first stage, unclipped, keeps the lead defined and alive under the sustain
        blend = ch == Channel.Lead ? Db(4.3) : 0;
        lows.LowShelf(fsOs, 300, ch == Channel.Lead ? 8 : 0);
        stageCount = cfg.Length;
        for (int i = 0; i < cfg.Length; i++)
        {
            stages[i].Configure(fsOs, cfg[i].Item1, cfg[i].Item2);
            stages[i].Drive = cfg[i].Item3;
            stages[i].BiasShift = cfg[i].Item4;
        }
        brightCap.HighShelf(fsOs, 2200, (ch == Channel.Clean ? 5 : 3) + (bright ? 5 : 0));
    }

    bool acousticBlock;

    /// <summary>The acoustic channel's stereo side signal for the last block (see AcousticSim.Side); empty when
    /// the last block ran a tube channel.</summary>
    public ReadOnlySpan<float> AcousticSide(int n) => acousticBlock ? acoustic.Side(n) : ReadOnlySpan<float>.Empty;

    /// <summary>Processes a mono block in place.</summary>
    public void Process(Span<float> buf)
    {
        var p = P;
        if (p.Channel == Channel.Acoustic)
        {
            // acoustic: drive pedals still run oversampled, then the body simulator at the base rate
            foreach (var e in osPedals) e.Update();
            Span<double> ab = osBuf;
            for (int n = 0; n < buf.Length; n++)
            {
                os.Up(buf[n], ab);
                for (int k = 0; k < Oversampler.Factor; k++)
                    for (int e = 0; e < osPedals.Length; e++) ab[k] = osPedals[e].Run(ab[k]);
                buf[n] = (float)os.Down(ab);
            }
            acoustic.Process(buf, p);
            acousticBlock = true;
            return;
        }
        acousticBlock = false;
        if (p.Channel != lastCh || p.Bright != lastBright) { lastCh = p.Channel; lastBright = p.Bright; ConfigureChannel(lastCh, lastBright); }
        tone.Set(p.Treble.Value, p.Middle.Value, p.Bass.Value);
        if (p.Presence.Value != lastPres || p.Depth.Value != lastRes)
        {
            lastPres = p.Presence.Value; lastRes = p.Depth.Value;
            presence.HighShelf(fsOs, 3000, -3 + 11 * lastPres);
            resonance.Peak(fsOs, 95, 0.9, -2 + 10 * lastRes);
        }
        foreach (var e in osPedals) e.Update();

        double g = p.Gain.Value;
        double gainDb = lastCh switch { Channel.Clean => -10 + 30 * g, Channel.Crunch => -6 + 34 * g, _ => 34 * g };
        double tGain = Db(gainDb) * 4.0;               // DI level (~0.1-0.3 peak) -> grid volts scale
        double tMaster = Db(-16 + 26 * p.Master.Value); // power amp drive
        double chMakeup = lastCh == Channel.Clean ? 1.6 : 1.0;
        const double toneMakeup = 3.2;

        Span<double> ob = osBuf;
        for (int n = 0; n < buf.Length; n++)
        {
            sGain += smooth * (tGain - sGain);
            sMaster += smooth * (tMaster - sMaster);
            os.Up(buf[n], ob);
            for (int k = 0; k < Oversampler.Factor; k++)
            {
                double v = ob[k];
                for (int e = 0; e < osPedals.Length; e++) v = osPedals[e].Run(v);
                v *= sGain;
                double first = 0;
                for (int s = 0; s < stageCount; s++)
                {
                    v = stages[s].Process(v);
                    if (s == 0) { v = brightCap.Process(v); first = v; }
                }
                v -= first * blend; // one stage is inverted against the four
                if (lastCh == Channel.Lead) v = lows.Process(v);
                v = tone.Process(v * chMakeup) * toneMakeup;

                // power amp: presence/depth, push-pull saturation with supply sag
                v = resonance.Process(presence.Process(v));
                v = powerHp.Process(v) * sMaster;
                double ceiling = 1.0 / (1 + 0.6 * sagEnv);
                double y = ceiling * Math.Tanh(v / ceiling);
                double ay = Math.Abs(y);
                sagEnv += (ay > sagEnv ? sagAtk : sagRel) * (ay - sagEnv);
                ob[k] = y;
            }
            buf[n] = (float)os.Down(ob);
        }
    }
}
