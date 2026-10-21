using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using System.Windows.Forms;
using Jwala.Dsp;

namespace Jwala.UI;

static class Assets
{
    static readonly Dictionary<string, Image> cache = new();
    public static Image Image(string name)
    {
        if (cache.TryGetValue(name, out var img)) return img;
        using var s = typeof(Assets).Assembly.GetManifestResourceStream(name)
                      ?? throw new InvalidOperationException("missing asset " + name);
        using var tmp = System.Drawing.Image.FromStream(s);
        return cache[name] = new Bitmap(tmp);
    }
}

static class Gfx
{
    public static readonly Color Gold = Color.FromArgb(214, 178, 96);
    public static readonly Color Text = Color.FromArgb(235, 225, 205);
    public static readonly Color Dim = Color.FromArgb(150, 140, 125);

    public static GraphicsPath Round(RectangleF r, float rad)
    {
        var gp = new GraphicsPath(); float d = Math.Max(1, rad * 2);
        gp.AddArc(r.X, r.Y, d, d, 180, 90); gp.AddArc(r.Right - d, r.Y, d, d, 270, 90);
        gp.AddArc(r.Right - d, r.Bottom - d, d, d, 0, 90); gp.AddArc(r.X, r.Bottom - d, d, d, 90, 90);
        gp.CloseFigure(); return gp;
    }

    public static void Tile(Graphics g, Image img, RectangleF r, float scale = 1f)
    {
        using var tb = new TextureBrush(img, WrapMode.Tile);
        tb.ScaleTransform(scale, scale);
        g.FillRectangle(tb, r);
    }

    /// <summary>Draws an image scaled to cover the rect (like CSS background-size: cover).</summary>
    public static void Cover(Graphics g, Image img, Rectangle r)
    {
        float s = Math.Max((float)r.Width / img.Width, (float)r.Height / img.Height);
        float w = img.Width * s, h = img.Height * s;
        g.DrawImage(img, r.X + (r.Width - w) / 2, r.Y + (r.Height - h) / 2, w, h);
    }

    public static void Vignette(Graphics g, RectangleF r, int alpha)
    {
        using var gp = new GraphicsPath(); gp.AddEllipse(RectangleF.Inflate(r, r.Width * 0.25f, r.Height * 0.4f));
        using var pb = new PathGradientBrush(gp) { CenterColor = Color.Transparent, SurroundColors = new[] { Color.FromArgb(alpha, 0, 0, 0) } };
        g.FillRectangle(pb, r);
    }

    public static void Shadow(Graphics g, RectangleF r, float rad, int alpha = 120, float dx = 6, float dy = 10)
    {
        for (int i = 3; i >= 1; i--)
        {
            using var b = new SolidBrush(Color.FromArgb(alpha / (i + 1), 0, 0, 0));
            using var p = Round(new RectangleF(r.X + dx - i * 2, r.Y + dy - i * 2, r.Width + i * 4, r.Height + i * 4), rad + i * 2);
            g.FillPath(b, p);
        }
    }

    public static void Logo(Graphics g, RectangleF r, float size, StringAlignment align = StringAlignment.Center)
    {
        using var f = new Font("Segoe Script", size, FontStyle.Bold | FontStyle.Italic);
        var sf = new StringFormat { Alignment = align, LineAlignment = StringAlignment.Center };
        using (var sh = new SolidBrush(Color.FromArgb(170, 0, 0, 0)))
            g.DrawString("Jwala", f, sh, new RectangleF(r.X + size / 14, r.Y + size / 11, r.Width, r.Height), sf);
        using var gb = new LinearGradientBrush(r, Color.FromArgb(255, 240, 200), Color.FromArgb(176, 128, 52), 90f);
        g.DrawString("Jwala", f, gb, r, sf);
    }

    public static void Led(Graphics g, RectangleF r, bool on, Color c)
    {
        if (on)
        {
            var gr = RectangleF.Inflate(r, r.Width * 0.9f, r.Height * 0.9f);
            using var gp = new GraphicsPath(); gp.AddEllipse(gr);
            using var glow = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(190, c), SurroundColors = new[] { Color.FromArgb(0, c) } };
            g.FillEllipse(glow, gr);
        }
        using (var b = new SolidBrush(on ? c : Color.FromArgb(255, c.R / 4, c.G / 4, c.B / 4))) g.FillEllipse(b, r);
        using (var hl = new SolidBrush(Color.FromArgb(on ? 200 : 70, 255, 255, 255)))
            g.FillEllipse(hl, r.X + r.Width * 0.25f, r.Y + r.Height * 0.18f, r.Width * 0.35f, r.Height * 0.3f);
        using var pen = new Pen(Color.FromArgb(140, 0, 0, 0), 1); g.DrawEllipse(pen, r);
    }

    /// <summary>LED cut from the concept: the unlit dot, or the concept's own glow (a white light layer, 3x the
    /// LED size) when lit. A tint recolours the glow (the delay's green tap LED).</summary>
    public static void LedGlow(Graphics g, RectangleF r, bool on, Image off, Color? tint)
    {
        if (!on) { g.DrawImage(off, r); return; }
        var gr = RectangleF.Inflate(r, r.Width, r.Height);
        var glow = Assets.Image("led_glow.png");
        if (tint is not Color c) { g.DrawImage(glow, gr); return; }
        using var ia = new ImageAttributes();
        ia.SetColorMatrix(new ColorMatrix(new[]
        {
            new[] { c.R / 255f, 0, 0, 0, 0f }, new[] { 0, c.G / 255f, 0, 0, 0f }, new[] { 0, 0, c.B / 255f, 0, 0f },
            new[] { 0, 0, 0, 1f, 0 }, new[] { 0, 0, 0, 0, 1f },
        }));
        g.DrawImage(glow, Rectangle.Round(gr), 0, 0, glow.Width, glow.Height, GraphicsUnit.Pixel, ia);
    }

    public static void Meter(Graphics g, RectangleF r, float peak)
    {
        float db = 20 * MathF.Log10(peak + 1e-6f);
        using (var bg = new SolidBrush(Color.FromArgb(14, 12, 10))) g.FillRectangle(bg, r);
        const int segs = 16;
        bool vertical = r.Height > r.Width;
        float len = (vertical ? r.Height : r.Width) - 2, sl = len / segs;
        for (int i = 0; i < segs; i++)
        {
            float segDb = -60 + i * 3.75f;
            Color c = i >= segs - 2 ? Color.FromArgb(255, 50, 40) : i >= segs - 5 ? Color.FromArgb(255, 190, 40) : Color.FromArgb(70, 220, 90);
            using var b = new SolidBrush(db >= segDb ? c : Color.FromArgb(35, c));
            if (vertical) g.FillRectangle(b, r.X + 1, r.Bottom - 1 - (i + 1) * sl + 1, r.Width - 2, sl - 1.5f);
            else g.FillRectangle(b, r.X + 1 + i * sl, r.Y + 1, sl - 1.5f, r.Height - 2);
        }
    }

    public static void Quality(Graphics g)
    {
        g.SmoothingMode = SmoothingMode.AntiAlias;
        g.InterpolationMode = InterpolationMode.HighQualityBicubic;
        g.PixelOffsetMode = PixelOffsetMode.HighQuality;
        // unhinted, with fractional advances: the glyph spacing of the Rust UI (fontdue)
        g.TextRenderingHint = TextRenderingHint.AntiAlias;
    }

    /// <summary>Draws text across exactly <paramref name="width"/> by spreading the extra space evenly
    /// between the letters (GDI+ has no letter-spacing).</summary>
    public static void DrawSpaced(Graphics g, string text, Font f, Brush brush, float x, float y, float width)
    {
        var tf = StringFormat.GenericTypographic;
        if (text.Length < 2) { g.DrawString(text, f, brush, x + (width - g.MeasureString(text, f, PointF.Empty, tf).Width) / 2, y, tf); return; }
        var w = new float[text.Length];
        float sum = 0;
        for (int i = 0; i < text.Length; i++) sum += w[i] = g.MeasureString(text[i].ToString(), f, PointF.Empty, tf).Width;
        float gap = (width - sum) / (text.Length - 1);
        for (int i = 0; i < text.Length; i++) { g.DrawString(text[i].ToString(), f, brush, x, y, tf); x += w[i] + gap; }
    }

    // Text is measured and placed without GDI+'s default 1/6 em padding, like the Rust UI: typographic formats.
    /// <summary>Top-left placement, for <c>DrawString(s, font, brush, x, y, Gfx.Typo)</c> and measuring.</summary>
    public static readonly StringFormat Typo = Format(StringAlignment.Near, StringAlignment.Near, false);
    public static readonly StringFormat Center = Format(StringAlignment.Center, StringAlignment.Center, false);
    public static readonly StringFormat CenterNoWrap = Format(StringAlignment.Center, StringAlignment.Center, true);

    /// <summary>Text in a rect (h/v: 0 near, 0.5 centre, 1 far) with symbols (◀ ▶ ♩ ▾ ♭ ♯ …) drawn from Segoe UI
    /// Symbol on the same baseline, like the Rust UI's per-glyph fallback; GDI+ would substitute other glyphs.</summary>
    public static void DrawText(Graphics g, string text, Font f, Brush brush, RectangleF r, float h = 0.5f, float v = 0.5f)
    {
        static bool Sym(char c) => c >= '←' && c <= '⯿';
        var runs = new List<(string S, bool Sym)>();
        foreach (char c in text)
            if (runs.Count > 0 && runs[^1].Sym == Sym(c)) runs[^1] = (runs[^1].S + c, Sym(c));
            else runs.Add((c.ToString(), Sym(c)));
        using var sym = new Font("Segoe UI Symbol", f.Size, f.Style & ~FontStyle.Bold, f.Unit);
        float Px(Font font, int metric) => font.SizeInPoints * g.DpiY / 72f * metric / font.FontFamily.GetEmHeight(font.Style);
        float asc = Px(f, f.FontFamily.GetCellAscent(f.Style)), lh = asc + Px(f, f.FontFamily.GetCellDescent(f.Style));
        var widths = new float[runs.Count];
        float total = 0;
        for (int i = 0; i < runs.Count; i++) total += widths[i] = g.MeasureString(runs[i].S, runs[i].Sym ? sym : f, PointF.Empty, Typo).Width;
        float x = r.X + (r.Width - total) * h, baseline = r.Y + (r.Height - lh) * v + asc;
        for (int i = 0; i < runs.Count; i++)
        {
            var font = runs[i].Sym ? sym : f;
            g.DrawString(runs[i].S, font, brush, x, baseline - Px(font, font.FontFamily.GetCellAscent(font.Style)), Typo);
            x += widths[i];
        }
    }

    public static StringFormat Format(StringAlignment h, StringAlignment v, bool noWrap, StringTrimming trim = StringTrimming.None)
    {
        var f = (StringFormat)StringFormat.GenericTypographic.Clone();
        f.Alignment = h; f.LineAlignment = v; f.Trimming = trim;
        if (noWrap) f.FormatFlags |= StringFormatFlags.NoWrap;
        return f;
    }
}

// =====================================================================  widgets

abstract class Widget
{
    public RectangleF Bounds;
    public bool Visible = true;
    public virtual RectangleF HitRect => Bounds;
    public abstract void Paint(Graphics g, bool hot, bool pressed);
    public virtual void Down(MouseEventArgs e, WidgetSurface s) { }
    public virtual void Drag(MouseEventArgs e, WidgetSurface s) { }
    public virtual void Up(MouseEventArgs e, WidgetSurface s) { }
    public virtual void Wheel(int delta, WidgetSurface s) { }
    public virtual void DoubleClick(WidgetSurface s) { }
    public virtual string Tooltip => null;
    /// <summary>Area to repaint when this widget changes (glows/bubbles extend past Bounds).</summary>
    public virtual RectangleF Dirty => RectangleF.Inflate(Bounds, 30, 40);
}

sealed class KnobWidget : Widget
{
    public readonly Param P;
    public Image Img;
    public Color Pointer = Color.FromArgb(250, 248, 240);
    /// <summary>The page bakes this knob's shadow into its background layer (SoftShadows), so draw none.</summary>
    public bool ShadowBaked;
    public bool Ticks; public Color TickColor = Color.FromArgb(40, 24, 8);
    PointF start; float startVal; bool dragging;

    public KnobWidget(Param p, Image img) { P = p; Img = img; }

    public override RectangleF HitRect => RectangleF.Inflate(Bounds, 6, 6);

    public override void Paint(Graphics g, bool hot, bool pressed)
    {
        var b = Bounds;
        float cx = b.X + b.Width / 2, cy = b.Y + b.Height / 2;
        if (Ticks)
        {
            float r0 = b.Width / 2 + 3, r1 = b.Width / 2 + Math.Max(6, b.Width * 0.12f);
            using var tp = new Pen(TickColor, 1.6f);
            for (int t = 0; t <= 10; t++)
            {
                double a = (-150 + 30 * t - 90) * Math.PI / 180;
                float len = t % 5 == 0 ? r1 + 3 : r1;
                g.DrawLine(tp, cx + (float)Math.Cos(a) * r0, cy + (float)Math.Sin(a) * r0, cx + (float)Math.Cos(a) * len, cy + (float)Math.Sin(a) * len);
            }
        }
        if (!ShadowBaked)
            using (var sh = new SolidBrush(Color.FromArgb(110, 0, 0, 0)))
                g.FillEllipse(sh, b.X + b.Width * 0.04f, b.Y + b.Height * 0.09f, b.Width, b.Height);
        g.DrawImage(Img, b);
        double ang = (-150 + 300 * P.Value - 90) * Math.PI / 180;
        float p0 = b.Width * 0.1f, p1 = b.Width * 0.44f;
        float x0 = cx + (float)Math.Cos(ang) * p0, y0 = cy + (float)Math.Sin(ang) * p0, x1 = cx + (float)Math.Cos(ang) * p1, y1 = cy + (float)Math.Sin(ang) * p1;
        float w = Math.Max(1.6f, b.Width * 0.035f);
        using (var pen = new Pen(Color.FromArgb(150, 0, 0, 0), w + 2.5f) { StartCap = LineCap.Round, EndCap = LineCap.Round }) g.DrawLine(pen, x0, y0, x1, y1);
        using (var pen = new Pen(Pointer, w) { StartCap = LineCap.Round, EndCap = LineCap.Round }) g.DrawLine(pen, x0, y0, x1, y1);

        if (dragging || hot)
        {
            using var f = new Font("Segoe UI", 8.5f, FontStyle.Bold);
            var txt = P.Display;
            float tw = g.MeasureString(txt, f, PointF.Empty, Gfx.Typo).Width;
            var rr = new RectangleF(cx - tw / 2 - 5, b.Bottom + 4, tw + 10, 17);
            using var bg = new SolidBrush(Color.FromArgb(dragging ? 235 : 190, 18, 15, 12));
            using var rp = Gfx.Round(rr, 5); g.FillPath(bg, rp);
            using var tb = new SolidBrush(Gfx.Gold);
            g.DrawString(txt, f, tb, rr, Gfx.CenterNoWrap);
        }
    }

    public override void Down(MouseEventArgs e, WidgetSurface s) { start = e.Location; startVal = P.Value; dragging = true; }
    public override void Drag(MouseEventArgs e, WidgetSurface s)
    {
        float sens = (Control.ModifierKeys & Keys.Shift) != 0 ? 0.0012f : 0.005f;
        P.Value = Math.Clamp(startVal + (start.Y - e.Y + (e.X - start.X) * 0.5f) * sens, 0, 1);
        s.Changed(this);
    }
    public override void Up(MouseEventArgs e, WidgetSurface s) { dragging = false; s.Changed(this); }
    public override void Wheel(int delta, WidgetSurface s)
    {
        float step = (Control.ModifierKeys & Keys.Shift) != 0 ? 0.005f : 0.02f;
        P.Value = Math.Clamp(P.Value + Math.Sign(delta) * step, 0, 1);
        s.Changed(this);
    }
    public override void DoubleClick(WidgetSurface s) { P.Value = P.Default; s.Changed(this); }
}

/// <summary>Rectangular latching switch with an LED (amp channel buttons, cab models…).</summary>
sealed class SwitchWidget : Widget
{
    public string Text; public Func<bool> Get; public Action Click; public Color LedColor = Color.FromArgb(255, 170, 40);
    public float FontSize = 7.5f;
    public override void Paint(Graphics g, bool hot, bool pressed)
    {
        var r = Bounds; bool on = Get();
        using var path = Gfx.Round(r, 5);
        using (var bg = new LinearGradientBrush(r, on ? Color.FromArgb(66, 58, 48) : hot ? Color.FromArgb(46, 41, 36) : Color.FromArgb(34, 30, 26), Color.FromArgb(12, 10, 8), 90f))
            g.FillPath(bg, path);
        using (var pen = new Pen(on ? Color.FromArgb(120, LedColor) : Color.FromArgb(140, 0, 0, 0), 1.2f)) g.DrawPath(pen, path);
        Gfx.Led(g, new RectangleF(r.X + 8, r.Y + r.Height / 2 - 3.5f, 7, 7), on, LedColor);
        using var f = new Font("Arial", FontSize, FontStyle.Bold);
        using var tb = new SolidBrush(on ? Color.FromArgb(245, 235, 210) : Gfx.Dim);
        g.DrawString(Text, f, tb, new RectangleF(r.X + 16, r.Y, r.Width - 18, r.Height), Gfx.CenterNoWrap);
    }
    public override void Down(MouseEventArgs e, WidgetSurface s) { Click(); s.Changed(this); }
}

/// <summary>Flat UI button (header, cab page).</summary>
sealed class ButtonWidget : Widget
{
    public string Text; public Action<WidgetSurface, RectangleF> Click; public Func<bool> Active = () => false;
    public float FontSize = 8.5f; public bool Bold = true;
    public override void Paint(Graphics g, bool hot, bool pressed)
    {
        var r = Bounds; bool on = Active();
        using var path = Gfx.Round(r, 5);
        using (var bg = new SolidBrush(pressed ? Color.FromArgb(70, 62, 50) : on ? Color.FromArgb(78, 64, 36) : hot ? Color.FromArgb(56, 50, 44) : Color.FromArgb(40, 36, 32)))
            g.FillPath(bg, path);
        using (var pen = new Pen(on ? Gfx.Gold : Color.FromArgb(80, 72, 60), 1)) g.DrawPath(pen, path);
        using var f = new Font("Segoe UI", FontSize, Bold ? FontStyle.Bold : FontStyle.Regular);
        using var tb = new SolidBrush(on ? Color.FromArgb(255, 230, 170) : Gfx.Text);
        Gfx.DrawText(g, Text, f, tb, r);
    }
    public override void Down(MouseEventArgs e, WidgetSurface s) { Click(s, Bounds); s.Changed(this); }
}

/// <summary>Chrome stomp switch. Its shadow is baked into the page's background layer (SoftShadows).</summary>
sealed class FootswitchWidget : Widget
{
    public Action Click;
    public Image Img = Assets.Image("footswitch.png");
    public override RectangleF HitRect => RectangleF.Inflate(Bounds, 10, 10);
    public override void Paint(Graphics g, bool hot, bool pressed)
    {
        var r = pressed ? RectangleF.Inflate(Bounds, -2, -2) : Bounds;
        g.DrawImage(Img, r);
    }
    public override void Down(MouseEventArgs e, WidgetSurface s) { Click(); s.Changed(this); }
}

/// <summary>Arbitrary painted/clickable element.</summary>
sealed class CustomWidget : Widget
{
    public Action<Graphics, RectangleF, bool, bool> OnPaint;
    public Action<MouseEventArgs, WidgetSurface> OnDown, OnDrag;
    public RectangleF? Hit;
    /// <summary>How far the painting reaches past Bounds (e.g. LED bloom).</summary>
    public float Reach;
    public override RectangleF HitRect => Hit ?? Bounds;
    public override RectangleF Dirty => RectangleF.Inflate(Bounds, Math.Max(30, Reach), Math.Max(40, Reach));
    public override void Paint(Graphics g, bool hot, bool pressed) => OnPaint?.Invoke(g, Bounds, hot, pressed);
    public override void Down(MouseEventArgs e, WidgetSurface s) { OnDown?.Invoke(e, s); s.Changed(this); }
    public override void Drag(MouseEventArgs e, WidgetSurface s) { if (OnDrag != null) { OnDrag(e, s); s.Changed(this); } }
}

// =====================================================================  surface

/// <summary>
/// Double-buffered control with a cached static background and interactive widgets. Everything is laid out and
/// painted in logical (96 DPI) pixels and scaled by the monitor's DPI, like the Rust UI: both buffers are 96 DPI
/// bitmaps drawn through a scale transform, so text in points keeps its size relative to the layout.
/// </summary>
abstract class WidgetSurface : Control
{
    protected readonly List<Widget> W = new();
    Bitmap layer, frame; bool layerDirty = true;
    Widget hot, active;
    public event Action<Widget> ParamsChanged;

    protected WidgetSurface()
    {
        SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.UserPaint | ControlStyles.ResizeRedraw, true);
    }

    /// <summary>Physical pixels per logical pixel.</summary>
    public float UiScale => DeviceDpi / 96f;
    /// <summary>The client area in logical pixels.</summary>
    protected Rectangle View => new(0, 0, (int)MathF.Ceiling(Width / UiScale), (int)MathF.Ceiling(Height / UiScale));

    protected abstract void Arrange(Rectangle r);
    protected abstract void PaintStatic(Graphics g);
    protected virtual void PaintOverlay(Graphics g) { }

    /// <summary>Repaints a logical-pixel area.</summary>
    public void InvalidateArea(RectangleF r)
    {
        float s = UiScale;
        Invalidate(Rectangle.Ceiling(new RectangleF(r.X * s - 1, r.Y * s - 1, r.Width * s + 2, r.Height * s + 2)));
    }

    public void Changed(Widget w) { if (w != null) InvalidateArea(w.Dirty); ParamsChanged?.Invoke(w); }
    public void Relayout() { layerDirty = true; Invalidate(); }

    protected override void OnResize(EventArgs e) { base.OnResize(e); layerDirty = true; Invalidate(); }
    protected override void OnDpiChangedAfterParent(EventArgs e) { base.OnDpiChangedAfterParent(e); layerDirty = true; Invalidate(); }

    static Bitmap Buffer(int w, int h)
    {
        var b = new Bitmap(w, h, PixelFormat.Format32bppPArgb);
        b.SetResolution(96, 96);
        return b;
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        if (Width < 50 || Height < 50) return;
        float s = UiScale;
        if (layerDirty || layer == null || layer.Width != Width || layer.Height != Height)
        {
            W.Clear();
            Arrange(View);
            layer?.Dispose();
            layer = Buffer(Width, Height);
            using (var lg = Graphics.FromImage(layer)) { Gfx.Quality(lg); lg.ScaleTransform(s, s); PaintStatic(lg); }
            layerDirty = false;
        }
        if (frame == null || frame.Width != Width || frame.Height != Height) { frame?.Dispose(); frame = Buffer(Width, Height); }

        var clip = e.ClipRectangle;
        using (var g = Graphics.FromImage(frame))
        {
            g.SetClip(clip);
            g.CompositingMode = CompositingMode.SourceCopy;
            g.DrawImage(layer, clip, clip, GraphicsUnit.Pixel);
            g.CompositingMode = CompositingMode.SourceOver;
            Gfx.Quality(g);
            g.ScaleTransform(s, s);
            var lc = new RectangleF(clip.X / s, clip.Y / s, clip.Width / s, clip.Height / s);
            foreach (var w in W) if (w.Visible && w.Dirty.IntersectsWith(lc)) w.Paint(g, w == hot && active == null, w == active);
            PaintOverlay(g);
        }
        e.Graphics.DrawImage(frame, clip, clip, GraphicsUnit.Pixel);
    }

    /// <summary>The event with its position in logical pixels.</summary>
    MouseEventArgs L(MouseEventArgs e) => new(e.Button, e.Clicks, (int)MathF.Floor(e.X / UiScale), (int)MathF.Floor(e.Y / UiScale), e.Delta);

    Widget Hit(Point p)
    {
        for (int i = W.Count - 1; i >= 0; i--) if (W[i].Visible && W[i].HitRect.Contains(p)) return W[i];
        return null;
    }

    protected override void OnMouseDown(MouseEventArgs e)
    {
        base.OnMouseDown(e); Focus();
        if (e.Button != MouseButtons.Left) return;
        e = L(e);
        active = Hit(e.Location);
        if (active == null) return;
        Capture = true;
        active.Down(e, this);
        InvalidateArea(active?.Dirty ?? RectangleF.Empty);
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        base.OnMouseMove(e);
        e = L(e);
        if (active != null) { active.Drag(e, this); return; }
        var h = Hit(e.Location);
        if (h != hot)
        {
            if (hot != null) InvalidateArea(hot.Dirty);
            hot = h;
            if (hot != null) InvalidateArea(hot.Dirty);
            Cursor = hot != null ? Cursors.Hand : Cursors.Default;
        }
    }

    protected override void OnMouseLeave(EventArgs e)
    {
        base.OnMouseLeave(e);
        if (hot != null && active == null) { InvalidateArea(hot.Dirty); hot = null; }
    }

    protected override void OnMouseUp(MouseEventArgs e)
    {
        base.OnMouseUp(e);
        if (active == null) return;
        var a = active; active = null; Capture = false;
        a.Up(L(e), this);
        InvalidateArea(a.Dirty);
    }

    protected override void OnMouseDoubleClick(MouseEventArgs e)
    {
        base.OnMouseDoubleClick(e);
        Hit(L(e).Location)?.DoubleClick(this);
    }

    protected override void OnMouseWheel(MouseEventArgs e)
    {
        base.OnMouseWheel(e);
        Hit(L(e).Location)?.Wheel(e.Delta, this);
    }

    /// <summary>Called ~30x/s for live elements (meters, LEDs).</summary>
    public virtual void Tick() { }
}
