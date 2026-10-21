using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Linq;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>
/// Top bar: logo, preset browser, global input/gate/output, meters, power and settings;
/// below it the signal-chain navigation (Pedals → Amp → Cab → Effects).
/// </summary>
sealed class HeaderBar : WidgetSurface
{
    public const int Row1 = 66, Row2 = 50;
    readonly RigModel m;
    public Func<string> PresetName = () => "";
    public Func<bool> PowerOn = () => false;
    public Func<string> Status = () => "";
    public Func<Rig> Engine = () => null;
    public Func<double> CpuLoad = () => 0;
    public Func<bool> TunerOn = () => false;
    public event Action ToggleTuner;
    public int Page = 1;
    public event Action PrevPreset, NextPreset, SavePreset, TogglePower, OpenSettings;
    public event Action<RectangleF> PresetMenu;
    public event Action<int> PageSelected;
    RectangleF meterIn, meterL, meterR, statusRect;

    static readonly string[] Pages = { "PEDALS", "AMP", "CAB", "EFFECTS" };

    public HeaderBar(RigModel m) { this.m = m; Height = (int)MathF.Round((Row1 + Row2) * UiScale); BackColor = Color.FromArgb(16, 14, 12); }

    protected override void Arrange(Rectangle r)
    {
        // ---- row 1: settings and power, then meters and global knobs from the right
        float x = r.Width - 16;
        x -= 34; W.Add(new CustomWidget { Bounds = new RectangleF(x, 16, 34, 34), OnPaint = PaintGear, OnDown = (_, _) => OpenSettings?.Invoke() });
        x -= 44; W.Add(new CustomWidget { Bounds = new RectangleF(x, 16, 34, 34), OnPaint = PaintPower, OnDown = (_, _) => TogglePower?.Invoke() });
        x -= 14;
        meterR = new RectangleF(x, 12, 8, 42); meterL = new RectangleF(x - 11, 12, 8, 42);
        x -= 47;
        meterIn = new RectangleF(x, 12, 8, 42);
        x -= 16;
        var knobImg = Assets.Image("pedal_knob.png");
        foreach (var p in m.Amp.Globals.Reverse())
        {
            x -= 58;
            W.Add(new KnobWidget(p, knobImg) { Bounds = new RectangleF(x + 12, 7, 34, 34) });
        }


        // preset group (◀ name ▶ SAVE TUNER) centred between the logo and the global knobs
        float cx = (270 + x) / 2f - 40;
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx - 190, 17, 32, 32), Text = "◀", Click = (_, _) => PrevPreset?.Invoke() });
        W.Add(new CustomWidget
        {
            Bounds = new RectangleF(cx - 152, 17, 304, 32),
            OnPaint = (g, b, hot, _) =>
            {
                using var path = Gfx.Round(b, 6);
                using (var bg = new SolidBrush(hot ? Color.FromArgb(44, 40, 36) : Color.FromArgb(30, 27, 24))) g.FillPath(bg, path);
                using (var pen = new Pen(Color.FromArgb(110, Gfx.Gold), 1)) g.DrawPath(pen, path);
                using var f = new Font("Segoe UI", 10.5f, FontStyle.Bold);
                using var tb = new SolidBrush(Gfx.Text);
                g.DrawString(PresetName(), f, tb, new RectangleF(b.X + 10, b.Y, b.Width - 34, b.Height), Gfx.Format(StringAlignment.Center, StringAlignment.Center, true, StringTrimming.EllipsisCharacter));
                using var ab = new SolidBrush(Gfx.Gold);
                Gfx.DrawText(g, "▾", f, ab, new RectangleF(b.Right - 26, b.Y, 20, b.Height));
            },
            OnDown = (_, _) => PresetMenu?.Invoke(new RectangleF(cx - 152, 49, 304, 0)),
        });
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx + 158, 17, 32, 32), Text = "▶", Click = (_, _) => NextPreset?.Invoke() });
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx + 198, 17, 62, 32), Text = "SAVE", Click = (_, _) => SavePreset?.Invoke() });
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx + 268, 17, 76, 32), Text = "♩ TUNER", Click = (_, _) => ToggleTuner?.Invoke(), Active = () => TunerOn() });


        // ---- row 2: signal chain navigation
        float tw = 168, gap = 34, total = tw * 4 + gap * 3, tx = (r.Width - total) / 2;
        for (int i = 0; i < 4; i++)
        {
            int idx = i;
            var b = new RectangleF(tx + i * (tw + gap), Row1 + 6, tw, Row2 - 12);
            W.Add(new CustomWidget { Bounds = b, OnPaint = (g, bb, hot, _) => PaintTab(g, bb, idx, hot), OnDown = (_, _) => { Page = idx; PageSelected?.Invoke(idx); Invalidate(); } });
        }
        statusRect = new RectangleF(tx + total + 20, Row1, r.Width - (tx + total + 20) - 24, Row2);
    }

    void PaintTab(Graphics g, RectangleF b, int idx, bool hot)
    {
        bool active = Page == idx && !TunerOn();
        using var path = Gfx.Round(b, 6);
        using (var bg = new SolidBrush(active ? Color.FromArgb(58, 48, 30) : hot ? Color.FromArgb(38, 34, 30) : Color.FromArgb(26, 23, 20))) g.FillPath(bg, path);
        using (var pen = new Pen(active ? Gfx.Gold : Color.FromArgb(60, 54, 46), active ? 1.5f : 1)) g.DrawPath(pen, path);
        using var f = new Font("Segoe UI", 8.5f, FontStyle.Bold);
        using var tb = new SolidBrush(active ? Color.FromArgb(255, 230, 170) : Gfx.Text);
        g.DrawString(Pages[idx], f, tb, new RectangleF(b.X, b.Y + 2, b.Width, b.Height / 2), Gfx.CenterNoWrap);

        // mini chain status under the label
        using var sf = new Font("Segoe UI", 7f, FontStyle.Bold);
        var sub = new RectangleF(b.X, b.Y + b.Height / 2, b.Width, b.Height / 2 - 2);
        if (idx == 0 || idx == 3)
        {
            Effect[] fx = idx == 0 ? m.Pre : m.Post;
            float dw = 30, start = b.X + (b.Width - dw * fx.Length) / 2;
            for (int i = 0; i < fx.Length; i++)
            {
                var d = new RectangleF(start + i * dw + 3, sub.Y + 3, dw - 6, sub.Height - 6);
                using var dp = Gfx.Round(d, 3);
                using var db = new SolidBrush(fx[i].Enabled ? Color.FromArgb(200, 255, 120, 40) : Color.FromArgb(50, 255, 255, 255));
                g.FillPath(db, dp);
            }
        }
        else
        {
            string s = idx == 1 ? m.Amp.Channel.ToString().ToUpper() + (m.Amp.Bright ? " · BRIGHT" : "")
                                : !m.Cab.Enabled ? "BYPASSED" : m.Amp.Channel == Channel.Acoustic ? "MIC ONLY"
                                : m.Cab.Model == 3 ? "USER IR" : Convolver.Models[m.Cab.Model];
            using var sb = new SolidBrush(Gfx.Dim);
            g.DrawString(s, sf, sb, sub, Gfx.CenterNoWrap);
        }
    }

    void PaintPower(Graphics g, RectangleF b, bool hot, bool pressed)
    {
        bool on = PowerOn();
        Color c = on ? Color.FromArgb(90, 230, 110) : Color.FromArgb(230, 70, 50);
        if (on)
        {
            var gr = RectangleF.Inflate(b, 8, 8);
            using var gp = new GraphicsPath(); gp.AddEllipse(gr);
            using var glow = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(90, c), SurroundColors = new[] { Color.FromArgb(0, c) } };
            g.FillEllipse(glow, gr);
        }
        using (var bg = new SolidBrush(hot ? Color.FromArgb(50, 46, 42) : Color.FromArgb(32, 29, 26))) g.FillEllipse(bg, b);
        using var pen = new Pen(c, 2.4f) { StartCap = LineCap.Round, EndCap = LineCap.Round };
        var ir = RectangleF.Inflate(b, -9, -9);
        g.DrawArc(pen, ir, -60, 300);
        g.DrawLine(pen, b.X + b.Width / 2, ir.Y - 2, b.X + b.Width / 2, b.Y + b.Height / 2);
    }

    void PaintGear(Graphics g, RectangleF b, bool hot, bool pressed)
    {
        using (var bg = new SolidBrush(hot ? Color.FromArgb(50, 46, 42) : Color.FromArgb(32, 29, 26))) g.FillEllipse(bg, b);
        using var f = new Font("Segoe UI Symbol", 14f);
        using var tb = new SolidBrush(Gfx.Text);
        g.DrawString("⚙", f, tb, b, Gfx.Center);
    }

    static readonly string Version = "v" + typeof(HeaderBar).Assembly.GetName().Version.ToString(3);

    /// <summary>The app version, baseline-aligned just after the logo script.</summary>
    static void PaintVersion(Graphics g)
    {
        using var lf = new Font("Segoe Script", 20, FontStyle.Bold | FontStyle.Italic);
        using var vf = new Font("Arial", 14f, FontStyle.Bold);
        float Ascent(Font f) => f.GetHeight(g) * f.FontFamily.GetCellAscent(f.Style) / f.FontFamily.GetLineSpacing(f.Style);
        float baseline = 6 + (52 - lf.GetHeight(g)) / 2 + Ascent(lf);
        float x = 14 + g.MeasureString("Jwala", lf).Width + 2;
        using var b = new SolidBrush(Gfx.Dim);
        g.DrawString(Version, vf, b, x, baseline - Ascent(vf));
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        using (var bg = new LinearGradientBrush(new Rectangle(0, 0, r.Width, Row1), Color.FromArgb(34, 30, 27), Color.FromArgb(18, 16, 14), 90f))
            g.FillRectangle(bg, 0, 0, r.Width, Row1);
        using (var bg2 = new SolidBrush(Color.FromArgb(12, 11, 10))) g.FillRectangle(bg2, 0, Row1, r.Width, Row2);
        using (var line = new Pen(Color.FromArgb(70, Gfx.Gold))) { g.DrawLine(line, 0, Row1, r.Width, Row1); g.DrawLine(line, 0, r.Height - 1, r.Width, r.Height - 1); }
        Gfx.Logo(g, new RectangleF(14, 6, 260, 52), 20, StringAlignment.Near);
        PaintVersion(g);

        using var lf = new Font("Arial", 6.5f, FontStyle.Bold);
        using var lb = new SolidBrush(Gfx.Dim);
        foreach (var w in W)
            if (w is KnobWidget k)
                g.DrawString(k.P.Name, lf, lb, new RectangleF(k.Bounds.X - 14, k.Bounds.Bottom + 2, k.Bounds.Width + 28, 12), Gfx.CenterNoWrap);
        g.DrawString("IN", lf, lb, new RectangleF(meterIn.X - 8, meterIn.Bottom + 1, 24, 10), Gfx.CenterNoWrap);
        g.DrawString("OUT", lf, lb, new RectangleF(meterL.X - 12, meterL.Bottom + 1, 42, 10), Gfx.CenterNoWrap);

        // arrows between the chain tabs
        using var af = new Font("Segoe UI", 12f, FontStyle.Bold);
        using var ab = new SolidBrush(Color.FromArgb(120, Gfx.Gold));
        var tabs = W.Where(w => w is CustomWidget && w.Bounds.Y > Row1).ToList();
        for (int i = 0; i + 1 < tabs.Count; i++)
            g.DrawString("→", af, ab, new RectangleF(tabs[i].Bounds.Right, tabs[i].Bounds.Y, tabs[i + 1].Bounds.X - tabs[i].Bounds.Right, tabs[i].Bounds.Height), Gfx.Center);
        using var cf = new Font("Segoe UI", 7.5f, FontStyle.Bold);
        g.DrawString("SIGNAL CHAIN", cf, lb, 18, Row1 + 18, Gfx.Typo);
    }

    protected override void PaintOverlay(Graphics g)
    {
        var e = Engine();
        Gfx.Meter(g, meterIn, e?.InputPeak ?? 0);
        Gfx.Meter(g, meterL, e?.OutputPeakL ?? 0);
        Gfx.Meter(g, meterR, e?.OutputPeakR ?? 0);
        using var f = new Font("Segoe UI", 7.5f);
        using var tb = new SolidBrush(PowerOn() ? Gfx.Dim : Color.FromArgb(230, 120, 90));
        string s = Status() + (PowerOn() ? $"  ·  CPU {CpuLoad() * 100:0}%" : "");
        g.DrawString(s, f, tb, statusRect, Gfx.Format(StringAlignment.Far, StringAlignment.Center, false, StringTrimming.EllipsisCharacter));
    }

    public override void Tick()
    {
        InvalidateArea(RectangleF.Union(meterIn, meterR));
        InvalidateArea(statusRect);
    }
}
