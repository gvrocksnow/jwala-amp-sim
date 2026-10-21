using System;

namespace Jwala.Dsp;

/// <summary>A 0..1 knob value shared between UI (writer) and audio thread (reader).</summary>
public sealed class Param
{
    public readonly string Name;
    public readonly float Default;
    public float Value;
    public Func<float, string> Format;

    public Param(string name, float def, Func<float, string> format = null)
    {
        Name = name; Default = def; Value = def; Format = format ?? (v => (v * 10).ToString("0.0"));
    }

    public string Display => Format(Value);
}

public enum EffectSlot { Mono, Oversampled, Stereo }

public abstract class Effect
{
    public readonly string Id, Name;
    public readonly Param[] Params;
    public volatile bool Enabled;
    /// <summary>Double-width enclosure in the UI.</summary>
    public bool Wide;
    public virtual bool HasTap => false;
    public virtual void Tap() { }

    protected Effect(string id, string name, params Param[] ps) { Id = id; Name = name; Params = ps; }

    public Param this[string name] => Array.Find(Params, p => p.Name == name);

    public abstract EffectSlot Slot { get; }
    public abstract void Prepare(double fs);

    // ---- helpers
    protected static double Db(double db) => Math.Pow(10, db / 20);
    protected static double Coef(double fs, double seconds) => 1 - Math.Exp(-1 / (fs * seconds));
    protected static double Log(double v, double lo, double hi) => lo * Math.Pow(hi / lo, v);
}

/// <summary>Effect running one sample at a time, at either the base rate or the 4x oversampled rate.</summary>
public abstract class SampleEffect : Effect
{
    readonly EffectSlot slot;
    double mix, mixStep;
    protected SampleEffect(string id, string name, EffectSlot slot, params Param[] ps) : base(id, name, ps) => this.slot = slot;
    public override EffectSlot Slot => slot;
    public override void Prepare(double fs) { mixStep = 1 / (fs * 0.015); Init(fs); mix = Enabled ? 1 : 0; }
    protected abstract void Init(double fs);
    /// <summary>Called once per block so parameter-dependent coefficients are cheap.</summary>
    public virtual void Update() { }
    protected abstract double Tick(double x);

    /// <summary>Click-free bypass: crossfades between dry and processed.</summary>
    public double Run(double x)
    {
        double target = Enabled ? 1 : 0;
        if (mix == 0 && target == 0) return x;
        mix = mix < target ? Math.Min(target, mix + mixStep) : Math.Max(target, mix - mixStep);
        double y = Tick(x);
        return x + (y - x) * mix;
    }
}

/// <summary>Stereo effect working on blocks. Delay/reverb keep ringing out after being switched off.</summary>
public abstract class StereoEffect : Effect
{
    protected StereoEffect(string id, string name, params Param[] ps) : base(id, name, ps) { }
    public override EffectSlot Slot => EffectSlot.Stereo;
    public abstract void Process(Span<float> l, Span<float> r);
}

/// <summary>Fractional-delay line with cubic (Hermite) interpolation.</summary>
public sealed class DelayLine
{
    readonly float[] buf; int w; readonly int mask;
    public DelayLine(int minLength)
    {
        int n = 1; while (n < minLength + 4) n <<= 1;
        buf = new float[n]; mask = n - 1;
    }
    public int Capacity => buf.Length - 4;
    public void Write(double x) { w = (w + 1) & mask; buf[w] = (float)x; }
    public double Read(double delay)
    {
        if (delay < 1) delay = 1;
        if (delay > Capacity) delay = Capacity;
        double rp = w - delay; int i = (int)Math.Floor(rp); double f = rp - i;
        float xm1 = buf[(i - 1) & mask], x0 = buf[i & mask], x1 = buf[(i + 1) & mask], x2 = buf[(i + 2) & mask];
        double c1 = 0.5 * (x1 - xm1), c2 = xm1 - 2.5 * x0 + 2 * x1 - 0.5 * x2, c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        return ((c3 * f + c2) * f + c1) * f + x0;
    }
    public void Clear() => Array.Clear(buf);
}
