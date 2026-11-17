using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>
/// Full-page chromatic tuner on a glossy LED glass screen: dot-matrix note readout, LED cent meter and an LED strobe.
/// Laid out in a fixed design space (the cent meter spans x 0..900, the readout starts at y 0) and drawn scaled to fit
/// the page, centred. Mirrors the Rust UI's `tuner()`.
/// </summary>
sealed class TunerPanel : WidgetSurface
{
    readonly Tuner t;
    public Func<Rig> Engine = () => null;
    public Func<bool> PowerOn = () => false;
    public event Action CloseRequested;
    double needle, strobePhase; long lastTick = Environment.TickCount64;
    static readonly (string Name, double Midi)[] Strings = { ("E2", 40), ("A2", 45), ("D3", 50), ("G3", 55), ("B3", 59), ("E4", 64) };

    static readonly Color Green = Color.FromArgb(80, 255, 130), Amber = Color.FromArgb(255, 170, 40), Red = Color.FromArgb(255, 60, 50);
    static readonly Color White = Color.FromArgb(235, 235, 225), Grey = Color.FromArgb(150, 150, 150);

    // layout: design units -> logical px
    const float BlockLeft = -74, BlockTop = 38, BlockW = 1048, BlockH = 508;
    const float MeterW = 900, Cx = 450, MeterY = 254, StrobeY = 346, ButtonsY = 460;
    float s = 1, ox, oy;
    RectangleF screen;
    float X(float v) => ox + v * s;
    float Y(float v) => oy + v * s;
    RectangleF R(float x, float y, float w, float h) => new(X(x), Y(y), w * s, h * s);

    public TunerPanel(Tuner tuner) { t = tuner; BackColor = Color.Black; }

    protected override void Arrange(Rectangle r)
    {
        // keep clear of the TUNER title at the top left
        const float topPad = 70, sidePad = 30, bottomPad = 20;
        s = Math.Clamp(Math.Min((r.Width - 2 * sidePad) / BlockW, (r.Height - topPad - bottomPad) / BlockH), 0.4f, 2.5f);
        ox = r.X + (r.Width - BlockW * s) / 2 - BlockLeft * s;
        oy = Math.Max(r.Y + (r.Height - BlockH * s) / 2, r.Y + topPad) - BlockTop * s;
        screen = R(-68, 44, 1036, 356);
        ClearCaches();

        W.Add(new ButtonWidget { Bounds = R(Cx - 150, ButtonsY, 34, 32), Text = "◀", FontSize = 8.5f * s, Click = (_, _) => { t.ReferenceA = Math.Max(425, t.ReferenceA - 1); Invalidate(); } });
        W.Add(new CustomWidget
        {
            Bounds = R(Cx - 110, ButtonsY, 160, 32), Hit = RectangleF.Empty,
            OnPaint = (g, b, _, _) =>
            {
                using var f = new Font("Segoe UI", 10f * s, FontStyle.Bold);
                using var tb = new SolidBrush(Gfx.Text);
                g.DrawString($"A4 = {t.ReferenceA:0} Hz", f, tb, b, Gfx.Center);
            },
        });
        W.Add(new ButtonWidget { Bounds = R(Cx + 56, ButtonsY, 34, 32), Text = "▶", FontSize = 8.5f * s, Click = (_, _) => { t.ReferenceA = Math.Min(455, t.ReferenceA + 1); Invalidate(); } });
        W.Add(new SwitchWidget { Bounds = R(Cx + 110, ButtonsY + 2, 140, 28), Text = "MUTE OUTPUT", FontSize = 7.5f * s, Scale = s, Get = () => t.MuteOutput, Click = () => t.MuteOutput = !t.MuteOutput, LedColor = Color.FromArgb(255, 80, 60) });
        W.Add(new ButtonWidget { Bounds = R(Cx - 60, ButtonsY + 52, 120, 34), Text = "CLOSE TUNER", FontSize = 8.5f * s, Click = (_, _) => CloseRequested?.Invoke() });
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        using (var bg = new SolidBrush(Color.FromArgb(9, 9, 9))) g.FillRectangle(bg, r);
        using var hf = new Font("Segoe UI", 15f, FontStyle.Bold);
        using var title = new SolidBrush(Color.FromArgb(220, Gfx.Accent));
        g.DrawString("TUNER", hf, title, 28, 18, Gfx.Typo);
        using var sf = new Font("Segoe UI", 9f);
        using var dim = new SolidBrush(Color.FromArgb(160, 230, 220, 200));
        g.DrawString("Play a single string and let it ring.  T or Esc closes the tuner.", sf, dim, 31, 48, Gfx.Typo);

        // glossy glass screen with a chrome bezel
        Gfx.Shadow(g, new RectangleF(screen.X, screen.Y + 8 * s, screen.Width, screen.Height), 16 * s, 120, 0, 6 * s);
        var bezel = new[] { Color.FromArgb(30, 30, 33), Color.FromArgb(150, 150, 155), Color.FromArgb(70, 70, 74), Color.FromArgb(20, 20, 22) };
        for (int k = 0; k < bezel.Length; k++)
        {
            float i = k * 1.5f * s;
            using var pen = new Pen(bezel[k], 2 * s);
            using var bp = Gfx.Round(RectangleF.Inflate(screen, 6 * s - i, 6 * s - i), 16 * s - i);
            g.DrawPath(pen, bp);
        }
        using var sp = Gfx.Round(screen, 12 * s);
        var st = g.Save();
        g.SetClip(sp);
        g.InterpolationMode = InterpolationMode.HighQualityBicubic;
        g.DrawImage(Assets.Image("tuner_glass.jpg"), screen);
        g.Restore(st);
    }

    protected override void PaintOverlay(Graphics g)
    {
        bool power = PowerOn(), sig = power && t.HasSignal;
        var (note, oct, cents) = sig ? t.Describe(t.Frequency) : ("-", 0, 0.0);
        double ac = Math.Abs(cents);
        Color col = !sig ? Color.FromArgb(120, 120, 120) : ac < 3 ? Green : ac < 15 ? Amber : Red;

        // note: big dot-matrix letter with the octave beside it
        float nw = LedWidth(note, 30, 3);
        float nx = Cx - nw / 2;
        if (sig) Glow(g, R(Cx - 130, 34, 260, 160), col, 34);
        Led(g, note, nx, 156, 30, 3, col);
        if (sig) Led(g, oct.ToString(), nx + nw + 8, 156, 16, 2.5f, col);

        // frequency and cents
        if (sig)
        {
            string hz = $"{t.Frequency:0.00} HZ", ct = $"{(cents >= 0 ? "+" : "")}{cents:0.0} ¢";
            float w1 = LedWidth(hz, 12, 1.875f), w2 = LedWidth(ct, 12, 1.875f);
            float x0 = Cx - (w1 + 30 + w2) / 2;
            Led(g, hz, x0, 198, 12, 1.875f, White);
            Led(g, ct, x0 + w1 + 30, 198, 12, 1.875f, col);
        }
        else LedCentered(g, power ? "WAITING FOR A NOTE" : "POWER IS OFF - TURN THE AMP ON TO TUNE", Cx, 198, 12, 1.875f, Grey);

        // nearest standard-tuning string
        double midi = sig ? 69 + 12 * Math.Log2(t.Frequency / t.ReferenceA) : -100;
        int nearest = -1; double best = 1.5;
        for (int i = 0; i < Strings.Length; i++) if (Math.Abs(midi - Strings[i].Midi) < best) { best = Math.Abs(midi - Strings[i].Midi); nearest = i; }
        for (int i = 0; i < Strings.Length; i++)
        {
            float bx = Cx - 174 + i * 58 + 4;
            var b = R(bx, 208, 50, 24);
            bool on = i == nearest;
            if (on) Glow(g, RectangleF.Inflate(b, 10 * s, 10 * s), col, 40);
            using (var pen = new Pen(on ? Color.FromArgb(230, col) : Color.FromArgb(40, 255, 255, 255), 2 * s))
            using (var bp = Gfx.Round(b, 5 * s)) g.DrawPath(pen, bp);
            LedCentered(g, Strings[i].Name, bx + 25, 228, 11, 1.5f, on ? col : Grey);
        }

        // cent meter: 41 LED bars; the needle lights one bar and the trail back to centre glows faintly
        double target = sig ? Math.Clamp(cents, -50, 50) : 0;
        needle += (target - needle) * 0.35;
        for (int k = 0; k < 41; k++)
        {
            double c = -50 + k * 2.5;
            float x = k * (MeterW / 40), tall = k % 10 == 0 ? 44 : k == 20 ? 36 : 30;
            var bar = R(x - 7, MeterY + 44 - tall, 14, tall);
            bool lit = sig && Math.Abs(c - needle) <= 1.3;
            bool trail = sig && ((c > 0 && c <= needle) || (c < 0 && c >= needle));
            Color sc = MeterColor(c);
            Color fill = lit ? sc : k == 20 ? Color.FromArgb(70, Green) : trail ? Color.FromArgb(85, sc) : Color.FromArgb(16, 255, 255, 255);
            if (lit) Glow(g, RectangleF.Inflate(bar, 14 * s, 14 * s), sc, 90);
            using var bb = new SolidBrush(fill);
            using var bp = Gfx.Round(bar, 3 * s);
            g.FillPath(bb, bp);
        }
        foreach (var (c, lab) in new[] { (-50, "-50"), (-25, "-25"), (0, "0"), (25, "+25"), (50, "+50") })
            LedCentered(g, lab, (c + 50) / 100f * MeterW, MeterY + 68, 11, 1.5f, Color.FromArgb(170, 170, 165));
        var dark = Color.FromArgb(90, 90, 90);
        LedCentered(g, "b", -46, MeterY + 40, 19, 1.875f, sig && cents < -3 ? Amber : dark);
        LedCentered(g, "#", MeterW + 46, MeterY + 40, 19, 1.875f, sig && cents > 3 ? Amber : dark);

        // strobe: two rows of LED dots; blocks drift left when flat, right when sharp, stand still in tune
        long now = Environment.TickCount64; double dt = (now - lastTick) / 1000.0; lastTick = now;
        if (sig) strobePhase += cents * 2.2 * dt;
        long shift = (long)Math.Floor(strobePhase * 2);
        var litDot = DotSprite(Color.FromArgb(230, col), true);
        var offDot = DotSprite(Color.FromArgb(14, 255, 255, 255), false);
        for (int row = 0; row < 2; row++)
            for (int k = 0; k <= 90; k++)
            {
                bool on = sig && (((k - shift + row * 3) % 8) + 8) % 8 < 4;
                var spr = on ? litDot : offDot;
                float cx = X(k * 10), cy = Y(StrobeY + 4 + row * 14), half = spr.Size / 2;
                g.DrawImage(spr.Bmp, cx - half, cy - half, spr.Size, spr.Size);
            }

        // gloss: the glass sheen over the LEDs, so the light sits under the glass
        using var sp = Gfx.Round(screen, 12 * s);
        var st = g.Save();
        g.SetClip(sp, CombineMode.Intersect);
        g.DrawImage(Sheen(), screen);
        g.Restore(st);
    }

    public override void Tick()
    {
        var e = Engine();
        if (e != null && PowerOn()) t.Analyze(e.SampleRate);
        InvalidateArea(RectangleF.Inflate(screen, 8, 8));
    }

    static Color MeterColor(double c) { double a = Math.Abs(c); return a <= 5 ? Green : a <= 25 ? Amber : Red; }

    void Glow(Graphics g, RectangleF r, Color c, int alpha)
    {
        using var gp = new GraphicsPath(); gp.AddEllipse(r);
        using var b = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(alpha, c), SurroundColors = new[] { Color.FromArgb(0, c) } };
        g.FillEllipse(b, r);
    }

    // ------------------------------------------------------------------ LED dot matrix

    /// <summary>`text` rasterised `rows` cells tall (cap height) and thresholded, like the Rust `text::dot_mask`.</summary>
    sealed class DotMask { public int W, H, Baseline; public bool[] Cells; }

    static readonly Dictionary<(string, int), DotMask> masks = new();

    static DotMask Mask(string text, int rows)
    {
        if (masks.TryGetValue((text, rows), out var m)) return m;
        if (masks.Count > 400) masks.Clear();
        float px = rows * 1.32f;
        using var f = new Font("Arial", px, FontStyle.Bold, GraphicsUnit.Pixel);
        float ascent = px * f.FontFamily.GetCellAscent(FontStyle.Bold) / f.FontFamily.GetEmHeight(FontStyle.Bold);
        int baseRow = (int)MathF.Ceiling(ascent) + 2;
        int cw, ch = (int)MathF.Ceiling(px * 1.4f) + 6;
        using (var mg = Graphics.FromHwnd(IntPtr.Zero)) cw = (int)MathF.Ceiling(mg.MeasureString(text, f, PointF.Empty, StringFormat.GenericTypographic).Width) + 10;
        using var bmp = new Bitmap(Math.Max(1, cw), ch, PixelFormat.Format32bppArgb);
        using (var bg = Graphics.FromImage(bmp))
        {
            bg.Clear(Color.Black);
            bg.TextRenderingHint = TextRenderingHint.AntiAlias;
            bg.DrawString(text, f, Brushes.White, 2, baseRow - ascent, StringFormat.GenericTypographic);
        }
        var lit = new bool[cw * ch];
        var data = bmp.LockBits(new Rectangle(0, 0, cw, ch), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        unsafe
        {
            for (int y = 0; y < ch; y++)
            {
                byte* row = (byte*)data.Scan0 + y * data.Stride;
                for (int x = 0; x < cw; x++) lit[y * cw + x] = row[x * 4 + 1] > 110;
            }
        }
        bmp.UnlockBits(data);
        int x0 = cw, y0 = ch, x1 = -1, y1 = -1;
        for (int y = 0; y < ch; y++)
            for (int x = 0; x < cw; x++)
                if (lit[y * cw + x]) { x0 = Math.Min(x0, x); y0 = Math.Min(y0, y); x1 = Math.Max(x1, x); y1 = Math.Max(y1, y); }
        m = new DotMask();
        if (x1 >= 0)
        {
            m.W = x1 - x0 + 1; m.H = y1 - y0 + 1; m.Baseline = baseRow - y0; m.Cells = new bool[m.W * m.H];
            for (int y = 0; y < m.H; y++)
                for (int x = 0; x < m.W; x++) m.Cells[y * m.W + x] = lit[(y + y0) * cw + x + x0];
        }
        return masks[(text, rows)] = m;
    }

    /// <summary>Width of `text` as LED dots, in design units.</summary>
    static float LedWidth(string text, int rows, float pitch) => Mask(text, rows).W * pitch;

    /// <summary>A rendered LED string: the bitmap (device pixels) and where its baseline sits.</summary>
    sealed class LedSprite { public Bitmap Bmp; public float W, H, Baseline, Pad; }
    readonly Dictionary<(string, int, float, int), LedSprite> sprites = new();

    LedSprite Sprite(string text, int rows, float pitch, Color col)
    {
        var key = (text, rows, pitch, col.ToArgb());
        if (sprites.TryGetValue(key, out var sp)) return sp;
        if (sprites.Count > 300) { foreach (var v in sprites.Values) v.Bmp?.Dispose(); sprites.Clear(); }
        var m = Mask(text, rows);
        float p = pitch * s, d = p * (pitch < 2.5f ? 0.9f : 0.85f);
        bool glow = pitch >= 2.5f;
        float pad = glow ? d * 1.2f : 1;
        sp = new LedSprite { W = m.W * p, H = m.H * p, Baseline = m.Baseline * p, Pad = pad };
        float dev = UiScale;
        int bw = Math.Max(1, (int)MathF.Ceiling((sp.W + 2 * pad) * dev)), bh = Math.Max(1, (int)MathF.Ceiling((sp.H + 2 * pad) * dev));
        sp.Bmp = new Bitmap(bw, bh, PixelFormat.Format32bppPArgb);
        using var g = Graphics.FromImage(sp.Bmp);
        g.SmoothingMode = SmoothingMode.AntiAlias;
        g.ScaleTransform(dev, dev);
        using var dot = new SolidBrush(col);
        for (int y = 0; y < m.H; y++)
            for (int x = 0; x < m.W; x++)
            {
                if (!m.Cells[y * m.W + x]) continue;
                float cx = pad + (x + 0.5f) * p, cy = pad + (y + 0.5f) * p;
                if (glow)
                {
                    float gr = d * 1.6f;
                    using var gp = new GraphicsPath(); gp.AddEllipse(cx - gr, cy - gr, gr * 2, gr * 2);
                    using var gb = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(70, col), SurroundColors = new[] { Color.FromArgb(0, col) } };
                    g.FillPath(gb, gp);
                }
                g.FillEllipse(dot, cx - d / 2, cy - d / 2, d, d);
            }
        return sprites[key] = sp;
    }

    /// <summary>Draws `text` as LED dots with its left edge at `x` and baseline at `baseline` (design units).</summary>
    void Led(Graphics g, string text, float x, float baseline, int rows, float pitch, Color col)
    {
        var sp = Sprite(text, rows, pitch, col);
        if (sp.W <= 0) return;
        float left = X(x) - sp.Pad, top = Y(baseline) - sp.Baseline - sp.Pad;
        g.DrawImage(sp.Bmp, left, top, sp.W + 2 * sp.Pad, sp.H + 2 * sp.Pad);
    }

    void LedCentered(Graphics g, string text, float cx, float baseline, int rows, float pitch, Color col) =>
        Led(g, text, cx - LedWidth(text, rows, pitch) / 2, baseline, rows, pitch, col);

    sealed class DotSpriteBmp { public Bitmap Bmp; public float Size; }
    readonly Dictionary<(int, bool), DotSpriteBmp> dots = new();

    /// <summary>One strobe LED (7 design px), with a halo when lit.</summary>
    DotSpriteBmp DotSprite(Color col, bool lit)
    {
        if (dots.TryGetValue((col.ToArgb(), lit), out var d)) return d;
        float dot = 7 * s, size = lit ? dot * 3.2f : dot + 2, dev = UiScale;
        int px = Math.Max(1, (int)MathF.Ceiling(size * dev));
        var bmp = new Bitmap(px, px, PixelFormat.Format32bppPArgb);
        using (var g = Graphics.FromImage(bmp))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.ScaleTransform(dev, dev);
            float c = size / 2;
            if (lit)
            {
                using var gp = new GraphicsPath(); gp.AddEllipse(0, 0, size, size);
                using var gb = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(70, col), SurroundColors = new[] { Color.FromArgb(0, col) } };
                g.FillPath(gb, gp);
            }
            using var b = new SolidBrush(col);
            g.FillEllipse(b, c - dot / 2, c - dot / 2, dot, dot);
        }
        return dots[(col.ToArgb(), lit)] = new DotSpriteBmp { Bmp = bmp, Size = size };
    }

    static Bitmap sheen;
    /// <summary>The glass sheen as white light with its brightness in alpha (GDI+ has no additive blend).</summary>
    static Bitmap Sheen()
    {
        if (sheen != null) return sheen;
        var src = (Bitmap)Assets.Image("tuner_sheen.jpg");
        var b = new Bitmap(src.Width, src.Height, PixelFormat.Format32bppArgb);
        var sd = src.LockBits(new Rectangle(0, 0, src.Width, src.Height), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        var bd = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
        unsafe
        {
            for (int y = 0; y < b.Height; y++)
            {
                byte* sr = (byte*)sd.Scan0 + y * sd.Stride, dr = (byte*)bd.Scan0 + y * bd.Stride;
                for (int x = 0; x < b.Width; x++)
                {
                    byte a = Math.Max(sr[x * 4], Math.Max(sr[x * 4 + 1], sr[x * 4 + 2]));
                    dr[x * 4] = dr[x * 4 + 1] = dr[x * 4 + 2] = 255; dr[x * 4 + 3] = a;
                }
            }
        }
        src.UnlockBits(sd); b.UnlockBits(bd);
        return sheen = b;
    }

    void ClearCaches()
    {
        foreach (var v in sprites.Values) v.Bmp?.Dispose();
        sprites.Clear();
        foreach (var v in dots.Values) v.Bmp.Dispose();
        dots.Clear();
    }
}
