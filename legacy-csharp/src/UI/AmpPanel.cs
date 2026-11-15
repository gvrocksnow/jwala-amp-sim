using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Linq;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>Amp page: the rendered head on its cab, on the pedal pages' slate, with the controls laid over the
/// head's faceplate.</summary>
sealed class AmpPanel : WidgetSurface
{
    readonly RigModel m;
    RectangleF head, cab, power;
    public Func<bool> PowerOn = () => false;
    public event Action PowerClicked;

    static readonly Color Slate = Color.FromArgb(42, 45, 50);
    static readonly Color Print = Color.FromArgb(235, 232, 230, 222);

    // Control positions on the amp head as fractions of amp_head.png (measured by tools/cut_amp.py on the
    // Qwen Image 2.1 render made from concepts/rect_C_2.png; knobs since packed closer to make room for the
    // acoustic switches).
    static readonly float[] KnobX = { 0.2751f, 0.3501f, 0.4251f, 0.5001f, 0.5751f, 0.6501f, 0.7251f };
    const float RowY = 0.4131f, KnobD = 0.056f, LedD = 0.019f;
    /// <summary>CLEAN, CRUNCH, LEAD, ACOUSTIC on the left; the acoustic channel's DECAY, ATTACK and DYNAMIC
    /// switches, BRIGHT and POWER on the right.</summary>
    static readonly float[] ChX = { 0.092f, 0.137f, 0.182f, 0.227f };
    static readonly float[] AcousticX = { 0.785f, 0.821f, 0.857f };
    const float BrightX = 0.899f, PowerX = 0.937f;
    /// <summary>Cabinet width relative to the head in the stack render.</summary>
    const float CabW = 1.15f;

    /// <summary>Faceplate labels for the amp knobs (the head is narrow at small window sizes).</summary>
    static readonly string[] KnobNames = { "GAIN", "BASS", "MID", "TREBLE", "PRESENCE", "DEPTH", "MASTER" };
    /// <summary>The same knobs on the ACOUSTIC channel (see Dsp/Acoustic.cs).</summary>
    static readonly string[] AcousticKnobNames = { "BODY", "BASS", "MID", "TREBLE", "AIR", "RESO", "LEVEL" };
    static readonly string[] Channels = { "CLEAN", "CRUNCH", "LEAD", "ACOUSTIC" };
    /// <summary>The acoustic channel's switches (see Dsp/Acoustic.cs), as printed under their LEDs.</summary>
    static readonly string[] AcousticSwitches = { "DECAY", "ATTACK", "DYNAMIC" };

    public AmpPanel(RigModel m) { this.m = m; BackColor = Slate; }

    PointF At(float fx, float fy) => new(head.X + head.Width * fx, head.Y + head.Height * fy);
    static RectangleF Centered(PointF c, float w, float h) => new(c.X - w / 2, c.Y - h / 2, w, h);
    float Kd => head.Width * KnobD;
    float Ld => head.Width * LedD;

    protected override void Arrange(Rectangle r)
    {
        var headImg = Assets.Image("amp_head.png");
        var cabImg = Assets.Image("amp_cab.png");
        // the head as wide as fits (the cab is 15% wider), leaving room for the title above
        float hw = Math.Max(600, Math.Min(1400, (r.Width - 100) / CabW));
        float hh = hw * headImg.Height / headImg.Width;
        head = new RectangleF(r.X + (r.Width - hw) / 2, r.Y + 96, hw, hh);
        float cw = hw * CabW;
        cab = new RectangleF(r.X + (r.Width - cw) / 2, head.Bottom - hh * 0.005f, cw, cw * cabImg.Height / cabImg.Width);

        float kd = Kd, ld = Ld;
        var knobImg = Assets.Image("amp_knob.png");
        var knobs = m.Amp.HeadKnobs;
        for (int i = 0; i < knobs.Length; i++)
            W.Add(new KnobWidget(knobs[i], knobImg) { Bounds = Centered(At(KnobX[i], RowY), kd, kd), Ticks = true, TickColor = Color.FromArgb(150, Print), ShadowBaked = true });

        // channel LEDs: lit = the active channel
        var lit = new[]
        {
            ("amp_led_green.png", Color.FromArgb(60, 230, 110)), ("amp_led_red.png", Color.FromArgb(255, 170, 40)),
            ("amp_led_red.png", Color.FromArgb(255, 50, 40)), ("amp_led_green.png", Color.FromArgb(90, 180, 255)),
        };
        for (int i = 0; i < ChX.Length; i++)
        {
            int ch = i;
            var c = At(ChX[i], RowY);
            W.Add(new CustomWidget
            {
                Bounds = Centered(c, ld, ld), Reach = ld * 2.5f,
                Hit = Centered(new PointF(c.X, c.Y + ld * 0.6f), Math.Max(hw * 0.04f, ld * 2.2f), ld * 3.2f),
                OnPaint = (g, b, hot, _) =>
                {
                    bool on = (int)m.Amp.Channel == ch;
                    Led(g, b, on ? lit[ch] : null, hot);
                    // crunch is amber: warm the red lamp's core
                    if (on && ch == 1)
                        using (var core = new SolidBrush(Color.FromArgb(210, 255, 190, 70)))
                            g.FillEllipse(core, Centered(c, ld * 0.55f, ld * 0.55f));
                    // acoustic is blue: cool the green lamp's core
                    if (on && ch == 3)
                        using (var core = new SolidBrush(Color.FromArgb(210, 130, 200, 255)))
                            g.FillEllipse(core, Centered(c, ld * 0.55f, ld * 0.55f));
                },
                // the knob labels change with the acoustic channel, so repaint the faceplate
                OnDown = (_, _) => { m.Amp.Channel = (Channel)ch; Relayout(); },
            });
        }

        // the acoustic channel's switches, blue like its lamp
        for (int i = 0; i < AcousticX.Length; i++)
        {
            int sw = i;
            var c = At(AcousticX[i], RowY);
            W.Add(new CustomWidget
            {
                Bounds = Centered(c, ld, ld), Reach = ld * 2.5f, Hit = Centered(new PointF(c.X, c.Y + ld * 0.6f), ld * 1.8f, ld * 3.2f),
                OnPaint = (g, b, hot, _) =>
                {
                    bool on = sw switch { 0 => m.Amp.Decay, 1 => m.Amp.Attack, _ => m.Amp.Dynamic };
                    Led(g, b, on ? ("amp_led_green.png", Color.FromArgb(90, 180, 255)) : null, hot);
                    if (on)
                        using (var core = new SolidBrush(Color.FromArgb(210, 130, 200, 255)))
                            g.FillEllipse(core, Centered(c, ld * 0.55f, ld * 0.55f));
                },
                OnDown = (_, _) =>
                {
                    if (sw == 0) m.Amp.Decay = !m.Amp.Decay;
                    else if (sw == 1) m.Amp.Attack = !m.Amp.Attack;
                    else m.Amp.Dynamic = !m.Amp.Dynamic;
                },
            });
        }

        W.Add(new CustomWidget
        {
            Bounds = Centered(At(BrightX, RowY), ld, ld), Reach = ld * 2.5f, Hit = Centered(new PointF(At(BrightX, RowY).X, At(BrightX, RowY).Y + ld * 0.6f), ld * 1.8f, ld * 3.2f),
            OnPaint = (g, b, hot, _) => Led(g, b, m.Amp.Bright ? ("amp_led_red.png", Color.FromArgb(255, 50, 40)) : null, hot),
            OnDown = (_, _) => m.Amp.Bright = !m.Amp.Bright,
        });

        // power: the audio switch
        power = Centered(At(PowerX, RowY), ld, ld);
        W.Add(new CustomWidget
        {
            Bounds = power, Reach = ld * 2.5f, Hit = Centered(new PointF(At(PowerX, RowY).X, At(PowerX, RowY).Y + ld * 0.6f), ld * 1.8f, ld * 3.2f),
            OnPaint = (g, b, hot, _) => Led(g, b, PowerOn() ? ("amp_led_green.png", Color.FromArgb(60, 230, 110)) : null, hot),
            OnDown = (_, _) => PowerClicked?.Invoke(),
        });
    }

    /// <summary>A lit or unlit panel LED from the render; the colour tints the lit lamp's bloom. Hovered LEDs get
    /// a thin ring.</summary>
    void Led(Graphics g, RectangleF r, (string Img, Color Glow)? lit, bool hot)
    {
        float d = r.Width;
        if (lit is (string img, Color glow))
        {
            var gr = RectangleF.Inflate(r, d * 1.4f, d * 1.4f);
            using (var gp = new GraphicsPath())
            {
                gp.AddEllipse(gr);
                using var pb = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(120, glow), SurroundColors = new[] { Color.FromArgb(0, glow) }, FocusScales = new PointF(0.15f, 0.15f) };
                g.FillEllipse(pb, gr);
            }
            g.DrawImage(Assets.Image(img), r);
        }
        else g.DrawImage(Assets.Image("amp_led_off.png"), r);
        if (hot)
            using (var pen = new Pen(Color.FromArgb(120, Print), 1.2f))
                g.DrawEllipse(pen, RectangleF.Inflate(r, d * 0.35f, d * 0.35f));
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        using (var bg = new SolidBrush(Slate)) g.FillRectangle(bg, r);
        using (var tf = new Font("Segoe UI Semibold", 15f))
        using (var tb = new SolidBrush(Color.FromArgb(240, 242, 245)))
            g.DrawString("AMPLIFIER", tf, tb, 30, 20, Gfx.Typo);
        using (var sf = new Font("Segoe UI", 9f))
        using (var sb = new SolidBrush(Color.FromArgb(150, 205, 210, 220)))
            g.DrawString("Three tube channels plus an acoustic-guitar simulator — click a channel LED to switch, drag knobs (Shift = fine, double-click = reset)", sf, sb, 33, 52, Gfx.Typo);

        // soft shadows like the pedal pages: the stack on the slate, then the knobs and LEDs on the faceplate
        var headImg = Assets.Image("amp_head.png");
        var cabImg = Assets.Image("amp_cab.png");
        float u = head.Width / 1.6f;
        SoftShadows.Cast(g, r.Size, og => { og.DrawImage(cabImg, cab); og.DrawImage(headImg, head); }, null, new[]
        {
            new SoftShadows.Term(u * 0.012f, u * 0.006f, u * 0.01f, 0.6f),
            new SoftShadows.Term(u * 0.07f, u * 0.035f, u * 0.05f, 0.5f),
            new SoftShadows.Term(u * 0.16f, u * 0.08f, u * 0.16f, 0.4f),
        });
        g.DrawImage(cabImg, cab);
        g.DrawImage(headImg, head);

        float kd = Kd, ld = Ld;
        SoftShadows.Cast(g, r.Size, og =>
        {
            foreach (var k in W.OfType<KnobWidget>()) og.DrawImage(k.Img, k.Bounds);
            using var black = new SolidBrush(Color.Black);
            foreach (var fx in ChX.Concat(AcousticX).Append(BrightX).Append(PowerX))
                og.FillEllipse(black, Centered(At(fx, RowY), ld * 1.1f, ld * 1.1f));
        }, og => og.DrawImage(headImg, head), new[]
        {
            new SoftShadows.Term(kd * 0.05f, kd * 0.025f, kd * 0.04f, 0.7f),
            new SoftShadows.Term(kd * 0.2f, kd * 0.1f, kd * 0.12f, 0.55f),
            new SoftShadows.Term(kd * 0.4f, kd * 0.2f, kd * 0.3f, 0.35f),
        }, down: 1);

        // printing on the faceplate: labels above the controls, white like the real thing
        float lf = Math.Clamp(head.Width / 150f, 6f, 9.5f);
        float labelY = head.Y + head.Height * RowY - kd * 0.5f - lf * 2.6f;
        using var font = new Font("Arial", lf, FontStyle.Bold);
        using var small = new Font("Arial", lf * 0.82f, FontStyle.Bold);
        using var ink = new SolidBrush(Print);
        void Label(float x, string s) => g.DrawString(s, font, ink, new RectangleF(x - 60, labelY, 120, lf * 2), Gfx.CenterNoWrap);

        var names = m.Amp.Channel == Channel.Acoustic ? AcousticKnobNames : KnobNames;
        for (int i = 0; i < KnobX.Length; i++) Label(At(KnobX[i], RowY).X, names[i]);
        for (int i = 0; i < ChX.Length; i++)
        {
            var c = At(ChX[i], RowY);
            g.DrawString(Channels[i], small, ink, new RectangleF(c.X - 40, c.Y + ld * 0.8f, 80, lf * 2), Gfx.CenterNoWrap);
        }
        float c0 = At(ChX[0], RowY).X, c3 = At(ChX[3], RowY).X;
        g.DrawString("CHANNEL", font, ink, new RectangleF(c0, labelY, c3 - c0, lf * 2), Gfx.CenterNoWrap);
        // the acoustic channel's switches: dimmed print while another channel is selected
        using (var acInk = new SolidBrush(m.Amp.Channel == Channel.Acoustic ? Print : Color.FromArgb(110, Print)))
        {
            for (int i = 0; i < AcousticX.Length; i++)
            {
                var c = At(AcousticX[i], RowY);
                g.DrawString(AcousticSwitches[i], small, acInk, new RectangleF(c.X - 40, c.Y + ld * 0.8f, 80, lf * 2), Gfx.CenterNoWrap);
            }
            float a0 = At(AcousticX[0], RowY).X, a2 = At(AcousticX[2], RowY).X;
            g.DrawString("ACOUSTIC", font, acInk, new RectangleF(a0 - 20, labelY, a2 - a0 + 40, lf * 2), Gfx.CenterNoWrap);
        }
        foreach (var (fx, name) in new[] { (BrightX, "BRIGHT"), (PowerX, "POWER") })
        {
            var c = At(fx, RowY);
            g.DrawString(name, small, ink, new RectangleF(c.X - 40, c.Y + ld * 0.8f, 80, lf * 2), Gfx.CenterNoWrap);
        }
    }

    public void RefreshPower() => InvalidateArea(RectangleF.Inflate(power, power.Width * 3, power.Width * 3));
}
