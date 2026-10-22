using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Windows.Forms;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>Graphic EQ page: the rack unit (eq_panel.png, cut from concepts/eq_AR_3_level.png by tools/cut_eq.py) with
/// the live response curve on its display and nine band sliders plus LEVEL, laid out like the Rust version's page.</summary>
sealed class EqPage : WidgetSurface
{
    static readonly Color Slate = Color.FromArgb(42, 45, 50);
    static readonly Color Amber = Color.FromArgb(255, 186, 84), Core = Color.FromArgb(255, 224, 170);
    /// <summary>eq_panel.png: width / height.</summary>
    const float Aspect = 2.3309f;
    /// <summary>Inside the display's bezel, as fractions of the panel: x0, y0, x1, y1.</summary>
    static readonly float[] Glass = { 0.0729f, 0.1020f, 0.9263f, 0.4472f };
    /// <summary>The rows of values above the sliders and names below them, down the panel's height.</summary>
    const float ValueY = 0.462f, NameY = 0.925f;
    /// <summary>The response display spans ±this many dB.</summary>
    const float ViewDb = 18;

    readonly EqParams p;
    public Func<Rig> Engine = () => null;
    RectangleF unit, scr;
    float left, sw, y0, y1, vy, ny;
    CustomWidget curve;

    public EqPage(EqParams p)
    {
        this.p = p; BackColor = Slate;
        ParamsChanged += _ => { if (curve != null) InvalidateArea(curve.Bounds); };   // the curve follows the sliders
    }

    double Fs => Engine()?.SampleRate ?? 48000;
    float Fx(double f) => scr.X + scr.Width * (float)(Math.Log(f / 20) / Math.Log(1000));
    float Dy(double db) => scr.Y + scr.Height / 2 - (float)(db / ViewDb) * (scr.Height / 2 - 8);
    float Ty(float v) => y1 - v * (y1 - y0);
    float Cx(int i) => left + sw * (i + 0.5f + (i == EqParams.Bands ? 0.6f : 0));

    protected override void Arrange(Rectangle r)
    {
        // the rack unit, as large as fits under the title
        float top = r.Y + 84, bottom = r.Bottom - 30;
        float uw = Math.Min(r.Width - 120, 1300);
        if (uw / Aspect > bottom - top) uw = (bottom - top) * Aspect;
        float uh = uw / Aspect;
        unit = new RectangleF(r.X + (r.Width - uw) / 2, top + Math.Max(0, (bottom - top - uh) / 2), uw, uh);
        scr = RectangleF.FromLTRB(unit.X + Glass[0] * uw, unit.Y + Glass[1] * uh, unit.X + Glass[2] * uw, unit.Y + Glass[3] * uh);

        // sliders: nine bands, a gap, then LEVEL; the thumb's centre travels from just under the values to just over the names
        left = unit.X + 64; sw = (unit.Right - 22 - left) / (EqParams.Bands + 1.6f);
        vy = unit.Y + ValueY * uh; ny = unit.Y + NameY * uh;
        y0 = vy + 28; y1 = ny - 12;

        // EQ ON and FLAT, top right above the unit
        W.Add(new SwitchWidget { Bounds = new RectangleF(unit.Right - 84, 22, 84, 28), Text = "EQ ON", Get = () => p.Enabled, Click = () => p.Enabled = !p.Enabled, LedColor = Color.FromArgb(255, 150, 50) });
        W.Add(new ButtonWidget
        {
            Bounds = new RectangleF(unit.Right - 160, 22, 64, 28), Text = "FLAT", FontSize = 8,
            Click = (s, _) => { for (int i = 0; i < EqParams.Bands; i++) p.Knobs[i].Value = 0.5f; s.Relayout(); },
        });
        curve = new CustomWidget { Bounds = scr, Hit = RectangleF.Empty, OnPaint = (g, _, _, _) => PaintCurve(g) };
        W.Add(curve);
        for (int i = 0; i < p.Knobs.Length; i++)
            W.Add(new FaderWidget(p.Knobs[i], Cx(i), sw, y0, y1, vy, () => p.Enabled));
    }

    void PaintCurve(Graphics g)
    {
        var v = new float[p.Knobs.Length];
        for (int i = 0; i < v.Length; i++) v[i] = p.Knobs[i].Value;
        // at the running rate, so the top band's squeeze towards Nyquist shows as it sounds
        double fs = Fs;
        const int n = 180;
        var pts = new PointF[n + 1];
        for (int k = 0; k <= n; k++)
        {
            double f = 20 * Math.Pow(1000, (double)k / n);
            pts[k] = new PointF(Fx(f), Dy(Math.Clamp(GraphicEq.ResponseDb(v, fs, Math.Min(f, 0.499 * fs)), -40, 40)));
        }
        var clip = g.Clip;
        g.SetClip(RectangleF.Inflate(scr, -3, -3), CombineMode.Intersect);
        // a soft amber glow round a pale core, like the concept's display
        var passes = p.Enabled ? new[] { (14f, Color.FromArgb(18, Amber)), (6f, Color.FromArgb(60, Amber)), (2.2f, Core) }
                               : new[] { (2f, Color.FromArgb(150, Gfx.Dim)) };
        foreach (var (w, c) in passes)
        {
            using var pen = new Pen(c, w) { LineJoin = LineJoin.Round, StartCap = LineCap.Round, EndCap = LineCap.Round };
            g.DrawLines(pen, pts);
        }
        g.Clip = clip;
        if (!p.Enabled)
        {
            using var f = new Font("Segoe UI", 8f, FontStyle.Bold);
            using var b = new SolidBrush(Gfx.Dim);
            Gfx.DrawText(g, "BYPASSED", f, b, new RectangleF(scr.Right - 110, scr.Y + 8, 100, 14), 1f);
        }
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        using (var bg = new SolidBrush(Slate)) g.FillRectangle(bg, r);
        using (var tf = new Font("Segoe UI Semibold", 15f))
        using (var tb = new SolidBrush(Color.FromArgb(240, 242, 245)))
            g.DrawString("GRAPHIC EQ", tf, tb, 30, 20, Gfx.Typo);
        using (var sf0 = new Font("Segoe UI", 9f))
        using (var sb = new SolidBrush(Color.FromArgb(150, 205, 210, 220)))
            g.DrawString("Nine octave bands after the cab, before the post effects — drag a slider (double-click = 0 dB, wheel = fine steps)", sf0, sb, 33, 52, Gfx.Typo);

        // the rack unit and its soft shadow, like the cab
        var panel = Assets.Image("eq_panel.png");
        float u = unit.Width / 1.6f;
        SoftShadows.Cast(g, r.Size, og => og.DrawImage(panel, unit), null, new[]
        {
            new SoftShadows.Term(u * 0.008f, u * 0.012f, u * 0.01f, 0.6f),
            new SoftShadows.Term(u * 0.03f, u * 0.05f, u * 0.05f, 0.45f),
            new SoftShadows.Term(u * 0.06f, u * 0.1f, u * 0.14f, 0.35f),
        });
        g.DrawImage(panel, unit);

        // the display's grid: log frequency 20 Hz–20 kHz, ±18 dB
        using var gridFont = new Font("Segoe UI", 6.5f);
        using var gridText = new SolidBrush(Color.FromArgb(60, 255, 255, 255));
        foreach (int d in new[] { -12, -6, 0, 6, 12 })
        {
            float y = Dy(d);
            using (var lb = new SolidBrush(Color.FromArgb(d == 0 ? 36 : 12, 255, 255, 255))) g.FillRectangle(lb, scr.X + 4, y, scr.Width - 8, 1);
            Gfx.DrawText(g, d == 0 ? "0" : d.ToString("+0;-0"), gridFont, gridText, new RectangleF(scr.X + 8, y - 10, 30, 10), 0, 1);
        }
        using (var vb = new SolidBrush(Color.FromArgb(10, 255, 255, 255)))
            for (int i = 0; i < EqParams.Bands; i++)
            {
                float x = Fx(EqParams.Freqs[i]);
                g.FillRectangle(vb, x, scr.Y + 4, 1, scr.Height - 8);
                Gfx.DrawText(g, p.Knobs[i].Name, gridFont, gridText, new RectangleF(x - 20, scr.Bottom - 16, 40, 12));
            }

        // the sliders' dB scale, ticks, slots and names
        using var scaleFont = new Font("Arial", 6.5f, FontStyle.Bold);
        using var dim = new SolidBrush(Gfx.Dim);
        foreach (int d in new[] { -12, -6, 0, 6, 12 })
            Gfx.DrawText(g, d == 0 ? "0 dB" : d.ToString("+0;-0"), scaleFont, dim, new RectangleF(left - 46, Ty(0.5f + d / (2f * (float)EqParams.RangeDb)) - 6, 38, 12), 1);
        using var nameFont = new Font("Arial", 7f, FontStyle.Bold);
        using var gold = new SolidBrush(Gfx.Gold);
        for (int i = 0; i < p.Knobs.Length; i++)
        {
            bool level = i == EqParams.Bands;
            float cx = Cx(i);
            if (level)
                using (var sep = new SolidBrush(Color.FromArgb(30, 255, 255, 255))) g.FillRectangle(sep, cx - sw * 0.8f, y0 - 8, 1, y1 - y0 + 16);
            for (int t = 0; t <= 8; t++)
            {
                float y = y0 + (y1 - y0) * t / 8, half = t == 4 ? sw * 0.34f : t % 2 == 0 ? sw * 0.26f : sw * 0.18f;
                using var tb = new SolidBrush(Color.FromArgb(t == 4 ? 70 : 26, 255, 255, 255));
                g.FillRectangle(tb, cx - half, y, half * 2, 1);
            }
            var slot = new RectangleF(cx - 3, y0 - 4, 6, y1 - y0 + 8);
            using (var sp = Gfx.Round(slot, 3))
            {
                using (var sb = new SolidBrush(Color.FromArgb(4, 4, 4))) g.FillPath(sb, sp);
                using var pen = new Pen(Color.FromArgb(22, 255, 255, 255), 1); g.DrawPath(pen, sp);
            }
            string name = level ? "LEVEL" : p.Knobs[i].Name.Replace('K', 'k') + " Hz";
            Gfx.DrawText(g, name, nameFont, level ? gold : dim, new RectangleF(cx - sw / 2, ny, sw, 12));
        }
    }
}

/// <summary>An EQ slider: the thumb, the slot lit from 0 dB to it, and its value above. Drag it (or click the slot to
/// jump there), wheel for fine steps, double-click for 0 dB.</summary>
sealed class FaderWidget : Widget
{
    public const float ThumbH = 20;
    static readonly Color Amber = Color.FromArgb(255, 186, 84);
    public readonly Param P;
    readonly float cx, sw, y0, y1, vy;
    readonly Func<bool> on;
    float grab;

    public FaderWidget(Param p, float cx, float sw, float y0, float y1, float vy, Func<bool> on)
    {
        P = p; this.cx = cx; this.sw = sw; this.y0 = y0; this.y1 = y1; this.vy = vy; this.on = on;
        Bounds = RectangleF.FromLTRB(cx - sw / 2, vy, cx + sw / 2, y1 + ThumbH / 2 + 6);
    }

    float ThumbY => y1 - P.Value * (y1 - y0);
    public override RectangleF HitRect => new(cx - sw / 2, y0 - ThumbH / 2 - 2, sw, y1 - y0 + ThumbH + 4);
    public override RectangleF Dirty => RectangleF.Inflate(Bounds, 4, 4);

    public override void Paint(Graphics g, bool hot, bool pressed)
    {
        float ym = y1 - 0.5f * (y1 - y0), yt = ThumbY;
        if (on() && Math.Abs(yt - ym) > 0.5f)
        {
            using var fill = Gfx.Round(new RectangleF(cx - 2, Math.Min(ym, yt), 4, Math.Abs(yt - ym)), 2);
            using var b = new SolidBrush(Color.FromArgb(200, Amber)); g.FillPath(b, fill);
        }
        float tw = Math.Clamp(sw * 0.6f, 26, 46);
        var thumb = new RectangleF(cx - tw / 2, yt - ThumbH / 2, tw, ThumbH);
        using (var sh = Gfx.Round(new RectangleF(thumb.X + 2, thumb.Y + 4, tw, ThumbH), 4))
        using (var sb = new SolidBrush(Color.FromArgb(130, 0, 0, 0))) g.FillPath(sb, sh);
        using (var tp = Gfx.Round(thumb, 4))
        {
            using (var grad = new LinearGradientBrush(RectangleF.Inflate(thumb, 0, 1), Color.FromArgb(214, 208, 196), Color.FromArgb(108, 102, 94), 90f)) g.FillPath(grad, tp);
            using var pen = new Pen(hot || pressed ? Gfx.Gold : Color.FromArgb(160, 0, 0, 0), 1); g.DrawPath(pen, tp);
        }
        using (var grip = new SolidBrush(on() ? Color.FromArgb(40, 30, 20) : Color.FromArgb(70, 66, 60)))
            g.FillRectangle(grip, thumb.X + 4, thumb.Y + ThumbH / 2 - 1, tw - 8, 2);
        using var f = new Font("Segoe UI", 7.5f, FontStyle.Bold);
        using var tb = new SolidBrush(hot || pressed ? Gfx.Gold : Gfx.Text);
        Gfx.DrawText(g, P.Display, f, tb, new RectangleF(cx - sw / 2, vy, sw, 14));
    }

    void Set(float y, WidgetSurface s) { P.Value = Math.Clamp((y1 - (y - grab)) / Math.Max(1, y1 - y0), 0, 1); s.Changed(this); }

    // grabbed by the thumb: move relative to it; anywhere else on the slot: jump there
    public override void Down(MouseEventArgs e, WidgetSurface s) { grab = Math.Abs(e.Y - ThumbY) <= ThumbH / 2 + 2 ? e.Y - ThumbY : 0; Set(e.Y, s); }
    public override void Drag(MouseEventArgs e, WidgetSurface s) => Set(e.Y, s);
    public override void Up(MouseEventArgs e, WidgetSurface s) => s.Changed(this);
    public override void Wheel(int delta, WidgetSurface s)
    {
        float step = (Control.ModifierKeys & Keys.Shift) != 0 ? 0.005f : 0.02f;
        P.Value = Math.Clamp(P.Value + Math.Sign(delta) * step, 0, 1);
        s.Changed(this);
    }
    public override void DoubleClick(WidgetSurface s) { P.Value = P.Default; s.Changed(this); }
}
