using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Linq;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>
/// A row of pastel stomp boxes on a dark slate stage, built from parts cut out of concepts/pedals_C_1.png
/// (tools/cut_concept.py):
/// knobs, a bold two-line name, status LED and footswitch, with a caption row below.
/// </summary>
sealed class PedalPage : WidgetSurface
{
    /// <summary>Accent colour (matches the rendered pastel_&lt;id&gt;.png enclosure) and category caption per effect id.</summary>
    static readonly Dictionary<string, (Color Body, string Caption)> Style = new()
    {
        ["comp"] = (Color.FromArgb(227, 180, 179), "COMPRESSOR"),
        ["od"] = (Color.FromArgb(173, 211, 196), "OVERDRIVE"),
        ["dist"] = (Color.FromArgb(152, 191, 213), "DISTORTION"),
        ["fuzz"] = (Color.FromArgb(222, 204, 153), "FUZZ"),
        ["doubler"] = (Color.FromArgb(220, 180, 188), "DOUBLER"),
        ["chorus"] = (Color.FromArgb(157, 209, 205), "CHORUS"),
        ["phaser"] = (Color.FromArgb(234, 174, 147), "PHASER"),
        ["delay"] = (Color.FromArgb(217, 204, 186), "DELAY"),
        ["reverb"] = (Color.FromArgb(182, 174, 216), "REVERB"),
    };
    static readonly Color Ink = Color.FromArgb(225, 34, 36, 42);
    static readonly Color Slate = Color.FromArgb(42, 45, 50);

    readonly Effect[] fx;
    readonly string title, subtitle;
    readonly RectangleF[] bodies;
    float scale = 1, dividerY;
    RectangleF tapLed; Delay tapDelay;

    public PedalPage(Effect[] effects, string title, string subtitle)
    {
        fx = effects; this.title = title; this.subtitle = subtitle;
        bodies = new RectangleF[fx.Length];
        BackColor = Slate;
    }

    static (Color Body, string Caption) StyleOf(Effect e) =>
        Style.TryGetValue(e.Id, out var st) ? st : (Color.FromArgb(190, 190, 196), e.Name);

    protected override void Arrange(Rectangle area)
    {
        // concept proportions: a small pedal is ~12% of the page width with ~0.9 pedal-widths between pedals;
        // the five-pedal effects page tightens the gaps so its pedals stay about the same size
        const float baseH = 360, smallW = 200, wideW = 390, top = 100, bottom = 124;
        float gap = fx.Length > 4 ? 120 : 176;
        float total = fx.Sum(e => e.Wide ? wideW : smallW) + gap * (fx.Length - 1);
        float s = Math.Min(Math.Min((area.Width - 100) / total, (area.Height - top - bottom) / baseH), 0.117f * area.Width / smallW);
        s = scale = Math.Clamp(s, 0.45f, 1.45f);
        float x = area.X + (area.Width - total * s) / 2;
        float y = area.Y + top + (area.Height - top - bottom - baseH * s) / 2;
        dividerY = y + baseH * s + 30 * s;

        for (int i = 0; i < fx.Length; i++)
        {
            var e = fx[i];
            float w = (e.Wide ? wideW : smallW) * s, h = baseH * s;
            var b = new RectangleF(x, y, w, h);
            bodies[i] = b;
            x += w + gap * s;
            float u = Unit(b);

            var knobImg = Assets.Image("knob_" + e.Id + ".png");
            float ks = KnobSize(b);
            for (int k = 0; k < e.Params.Length; k++)
            {
                var c = KnobCenter(e, b, k);
                W.Add(new KnobWidget(e.Params[k], knobImg) { Bounds = new RectangleF(c.X - ks / 2, c.Y - ks / 2, ks, ks), ShadowBaked = true });
            }

            // bypass footswitch + LED (and a tap switch on the delay), placed as on the concept
            float fs = u * 0.29f;
            float fy = b.Y + h * 0.755f, ledY = b.Y + h * 0.615f;
            float fx1 = e.HasTap ? b.X + w * 0.3f : b.X + w / 2;
            float ledS = u * 0.085f;
            var eff = e;
            var fsImg = Assets.Image("footswitch_" + e.Id + ".png");
            var dotImg = Assets.Image("led_off_" + e.Id + ".png");
            W.Add(new FootswitchWidget
            {
                Bounds = new RectangleF(fx1 - fs / 2, fy - fs / 2, fs, fs), Img = fsImg,
                // the LED and the caption under the divider both follow the bypass state
                Click = () => { eff.Enabled = !eff.Enabled; Relayout(); },
            });
            W.Add(new CustomWidget
            {
                Bounds = new RectangleF(fx1 - ledS / 2, ledY - ledS / 2, ledS, ledS), Hit = RectangleF.Empty, Reach = ledS * 1.5f,
                OnPaint = (g, r, _, _) => Gfx.LedGlow(g, r, eff.Enabled, dotImg, null),
            });
            if (e.HasTap)
            {
                float fx2 = b.X + w * 0.7f;
                W.Add(new FootswitchWidget { Bounds = new RectangleF(fx2 - fs / 2, fy - fs / 2, fs, fs), Img = fsImg, Click = eff.Tap });
                tapLed = new RectangleF(fx2 - ledS / 2, ledY - ledS / 2, ledS, ledS);
                tapDelay = (Delay)e;
                W.Add(new CustomWidget
                {
                    Bounds = tapLed, Hit = RectangleF.Empty, Reach = ledS * 1.5f,
                    OnPaint = (g, r, _, _) =>
                    {
                        double period = Delay.TimeMs(tapDelay.Params[0].Value);
                        bool on = Environment.TickCount64 % (long)Math.Max(1, period) < 70;
                        Gfx.LedGlow(g, r, on, dotImg, Color.FromArgb(120, 255, 150));
                    }
                });
            }
        }
    }

    /// <summary>Width of a small pedal at this pedal's height: concept measurements are fractions of it,
    /// so parts keep their size on the stretched wide pedals.</summary>
    static float Unit(RectangleF b) => b.Height * 200f / 360f;
    static float KnobSize(RectangleF b) => Unit(b) * 0.24f;

    /// <summary>Knobs in two rows: small pedals 2 per row (odd one centred), wide pedals 4 + 3.</summary>
    static PointF KnobCenter(Effect e, RectangleF b, int k)
    {
        int n = e.Params.Length, perRow = e.Wide ? 4 : 2;
        int row = k / perRow, col = k % perRow, inRow = Math.Min(perRow, n - row * perRow);
        float cx = e.Wide
            ? b.X + b.Width * (0.5f + (col - (inRow - 1) / 2f) * 0.2f)
            : b.X + b.Width * (inRow == 1 ? 0.5f : col == 0 ? 0.28f : 0.72f);
        return new PointF(cx, b.Y + b.Height * (0.095f + row * 0.18f));
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        float s = scale;

        // the concept's flat slate panel
        using (var bg = new SolidBrush(Slate)) g.FillRectangle(bg, r);

        using (var tf = new Font("Segoe UI Semibold", 15f))
        using (var tb = new SolidBrush(Color.FromArgb(240, 242, 245)))
            g.DrawString(title, tf, tb, 30, 20, Gfx.Typo);
        using (var sf = new Font("Segoe UI", 9f))
        using (var sb = new SolidBrush(Color.FromArgb(150, 205, 210, 220)))
            g.DrawString(subtitle, sf, sb, 33, 52, Gfx.Typo);

        // soft shadows, rendered once per layout into this cached layer (see SoftShadows):
        // the pedals (and their ports) on the slate, then the knobs and footswitches on the pedal lids
        float u = Unit(bodies[0]);
        SoftShadows.Cast(g, r.Size, og => { for (int i = 0; i < fx.Length; i++) PaintEnclosure(og, fx[i], bodies[i]); }, null, new[]
        {
            new SoftShadows.Term(u * 0.024f, u * 0.012f, u * 0.015f, 0.6f),  // contact
            new SoftShadows.Term(u * 0.14f, u * 0.07f, u * 0.1f, 0.5f),     // penumbra: light from the upper left, shadow twice as far right as down
            new SoftShadows.Term(u * 0.32f, u * 0.16f, u * 0.32f, 0.4f),    // ambient falloff
        });
        for (int i = 0; i < fx.Length; i++) PaintEnclosure(g, fx[i], bodies[i]);
        SoftShadows.Cast(g, r.Size, og =>
        {
            foreach (var k in W.OfType<KnobWidget>()) og.DrawImage(k.Img, k.Bounds);
            foreach (var f in W.OfType<FootswitchWidget>()) og.DrawImage(f.Img, f.Bounds);
        }, og => { for (int i = 0; i < fx.Length; i++) og.DrawImage(Assets.Image("pastel_" + fx[i].Id + ".png"), bodies[i]); }, new[]
        {
            new SoftShadows.Term(u * 0.03f, u * 0.015f, u * 0.012f, 0.75f),  // contact
            new SoftShadows.Term(u * 0.14f, u * 0.07f, u * 0.06f, 0.65f),   // penumbra: twice as far right as down
            new SoftShadows.Term(u * 0.24f, u * 0.12f, u * 0.16f, 0.4f),    // ambient
        }, down: 1);
        for (int i = 0; i < fx.Length; i++) PaintPrint(g, fx[i], bodies[i], s);

        // divider and caption row
        using (var dp = new Pen(Color.FromArgb(40, 255, 255, 255), 1))
            g.DrawLine(dp, 40, dividerY, r.Width - 40, dividerY);
        // captions are sized to the page, not the pedals: big and bold like the concept, but one shared size
        // shrunk until every caption fits its slot (the space between neighbouring pedals' centres)
        var slots = new float[fx.Length];
        for (int i = 0; i < fx.Length; i++)
        {
            float cx = bodies[i].X + bodies[i].Width / 2;
            float left = i > 0 ? cx - (bodies[i - 1].X + bodies[i - 1].Width / 2) : 2 * (cx - 20);
            float right = i + 1 < fx.Length ? bodies[i + 1].X + bodies[i + 1].Width / 2 - cx : 2 * (r.Width - 20 - cx);
            slots[i] = Math.Min(left, right) * 0.92f;
        }
        float cs = Math.Clamp(r.Width / 80f, 11f, 22f);
        using (var probe = new Font("Segoe UI", 20f, FontStyle.Bold))
            for (int i = 0; i < fx.Length; i++)
                cs = Math.Min(cs, 20f * slots[i] / g.MeasureString(StyleOf(fx[i]).Caption, probe, PointF.Empty, Gfx.Typo).Width);
        using var cf = new Font("Segoe UI", Math.Max(6f, cs), FontStyle.Bold);
        for (int i = 0; i < fx.Length; i++)
        {
            var e = fx[i]; var b = bodies[i];
            var cr = new RectangleF(b.X + b.Width / 2 - slots[i] / 2, dividerY + 12, slots[i], cf.GetHeight(g) * 1.1f);
            using var cb = new SolidBrush(e.Enabled ? Color.FromArgb(245, 246, 248) : Color.FromArgb(105, 245, 246, 248));
            g.DrawString(StyleOf(e).Caption, cf, cb, cr, Gfx.CenterNoWrap);
            if (e.Enabled)
            {
                float uw = Math.Min(b.Width * 0.3f, 46 * s);
                using var ub = new SolidBrush(StyleOf(e).Body);
                using var up = Gfx.Round(new RectangleF(b.X + (b.Width - uw) / 2, cr.Bottom + 4 * s, uw, Math.Max(2, 3 * s)), 1.5f * s);
                g.FillPath(ub, up);
            }
        }
    }

    /// <summary>The enclosure: side chrome ports cut from the concept, placed where they sit on it (fractions of a
    /// small pedal's width) and butting up against the pastel body drawn over them.</summary>
    static void PaintEnclosure(Graphics g, Effect e, RectangleF b)
    {
        float u = Unit(b);
        float py = b.Y + b.Height * 0.44f;
        var left = Assets.Image("port_left.png");
        float lw = u * 0.069f, lph = lw * left.Height / left.Width;
        g.DrawImage(left, b.X - lw + 1, py - lph / 2, lw, lph);
        var right = Assets.Image("port_right.png");
        float rw = u * 0.064f, rh = rw * right.Height / right.Width;
        g.DrawImage(right, b.Right - 1, py - rh / 2, rw, rh);
        g.DrawImage(Assets.Image("pastel_" + e.Id + ".png"), b);
    }

    /// <summary>What's printed on the lid: knob labels, the pedal name, ON/TAP.</summary>
    static void PaintPrint(Graphics g, Effect e, RectangleF b, float s)
    {
        float u = Unit(b);
        using var ink = new SolidBrush(Ink);

        // knob labels under each knob: small and light, like the concept's printed legends
        float lf = Math.Max(6f, 7.5f * s);
        float kf = Math.Max(5.6f, 6.75f * s);
        float ks = KnobSize(b);
        // drawn at 80% size but letter-spaced out to the width the label has at full size
        using (var full = new Font("Segoe UI Semibold", kf))
        using (var f = new Font("Segoe UI Semibold", kf * 0.8f))
            for (int k = 0; k < e.Params.Length; k++)
            {
                var c = KnobCenter(e, b, k);
                string name = e.Params[k].Name;
                float width = g.MeasureString(name, full, PointF.Empty, StringFormat.GenericTypographic).Width;
                float top = c.Y + ks / 2 + (full.GetHeight(g) - f.GetHeight(g)) / 2 + kf * 0.75f;
                Gfx.DrawSpaced(g, name, f, ink, c.X - width / 2, top, width);
            }

        // the pedal name: one bold line, or for two words two lines with the top word thinner (as on the
        // concept). Every pedal on both pages uses GREEN DRIVE's size (two lines fitted to a small pedal), so the
        // names match; a name only shrinks below it if it couldn't otherwise fit its own pedal.
        var words = e.Name.Split(' ', 2);
        var nameR = new RectangleF(b.X + b.Width * 0.08f, b.Y + b.Height * 0.405f, b.Width * 0.84f, b.Height * 0.155f);
        var weights = words.Length == 2 ? new[] { FontStyle.Regular, FontStyle.Bold } : new[] { FontStyle.Bold };
        float nf = NameSize(g, b);
        for (int i = 0; i < words.Length; i++)
            using (var probe = new Font("Segoe UI", 20f, weights[i]))
                nf = Math.Min(nf, 20f * nameR.Width / g.MeasureString(words[i], probe, PointF.Empty, Gfx.Typo).Width);
        nf = Math.Max(8, nf);
        float lh = 0;
        using (var probe = new Font("Segoe UI", nf, FontStyle.Bold)) lh = probe.GetHeight(g) * 0.9f;
        float ny = nameR.Y + (nameR.Height - lh * words.Length) / 2;
        for (int i = 0; i < words.Length; i++, ny += lh)
            using (var f = new Font("Segoe UI", nf, weights[i]))
                g.DrawString(words[i], f, ink, new RectangleF(b.X, ny, b.Width, lh), Gfx.CenterNoWrap);

        if (e.HasTap)
        {
            using var f = new Font("Segoe UI", lf, FontStyle.Bold);
            float ly = b.Y + b.Height * 0.615f;
            g.DrawString("ON", f, ink, new RectangleF(b.X + b.Width * 0.3f + 10 * s, ly - lf, 60, lf * 2), Gfx.Format(StringAlignment.Near, StringAlignment.Center, false));
            g.DrawString("TAP", f, ink, new RectangleF(b.X + b.Width * 0.7f + 10 * s, ly - lf, 60, lf * 2), Gfx.Format(StringAlignment.Near, StringAlignment.Center, false));
        }
    }

    /// <summary>Font size of "GREEN DRIVE" (two lines) fitted to a small pedal of this height: the size every
    /// pedal name uses.</summary>
    static float NameSize(Graphics g, RectangleF b)
    {
        float s = b.Height / 360f;
        float nameW = Unit(b) * 0.84f, nameH = b.Height * 0.155f;
        float lhUnit;
        using (var unit = new Font("Segoe UI", 1f, FontStyle.Bold)) lhUnit = unit.GetHeight(g);
        float nf = Math.Min(28f * s, nameH / (2 * lhUnit * 0.9f));
        foreach (var (word, style) in new[] { ("GREEN", FontStyle.Regular), ("DRIVE", FontStyle.Bold) })
            using (var probe = new Font("Segoe UI", 20f, style))
                nf = Math.Min(nf, 20f * nameW / g.MeasureString(word, probe, PointF.Empty, Gfx.Typo).Width);
        return nf;
    }

    public override void Tick()
    {
        if (tapDelay != null) InvalidateArea(RectangleF.Inflate(tapLed, tapLed.Width * 1.6f, tapLed.Width * 1.6f));
    }
}
