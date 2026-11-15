using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>Full-page chromatic tuner: big note readout, cents needle and a strobe band.</summary>
sealed class TunerPanel : WidgetSurface
{
    readonly Tuner t;
    public Func<Rig> Engine = () => null;
    public Func<bool> PowerOn = () => false;
    public event Action CloseRequested;
    RectangleF meter, strobe, readout;
    double needle, strobePhase; long lastTick = Environment.TickCount64;
    static readonly (string Name, double Midi)[] Strings = { ("E2", 40), ("A2", 45), ("D3", 50), ("G3", 55), ("B3", 59), ("E4", 64) };

    static readonly Color InTune = Color.FromArgb(90, 235, 120), Off = Color.FromArgb(255, 176, 60), Far = Color.FromArgb(240, 90, 70);

    public TunerPanel(Tuner tuner) { t = tuner; BackColor = Color.Black; }

    protected override void Arrange(Rectangle r)
    {
        float w = Math.Min(900, r.Width - 80), cx = r.Width / 2f;
        readout = new RectangleF(cx - w / 2, r.Y + 30, w, 230);
        meter = new RectangleF(cx - w / 2, readout.Bottom + 20, w, 70);
        strobe = new RectangleF(cx - w / 2, meter.Bottom + 26, w, 34);
        float by = strobe.Bottom + 80;

        W.Add(new ButtonWidget { Bounds = new RectangleF(cx - 150, by, 34, 32), Text = "◀", Click = (_, _) => { t.ReferenceA = Math.Max(425, t.ReferenceA - 1); Invalidate(); } });
        W.Add(new CustomWidget
        {
            Bounds = new RectangleF(cx - 110, by, 160, 32), Hit = RectangleF.Empty,
            OnPaint = (g, b, _, _) =>
            {
                using var f = new Font("Segoe UI", 10f, FontStyle.Bold);
                using var tb = new SolidBrush(Gfx.Text);
                g.DrawString($"A4 = {t.ReferenceA:0} Hz", f, tb, b, Gfx.Center);
            },
        });
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx + 56, by, 34, 32), Text = "▶", Click = (_, _) => { t.ReferenceA = Math.Min(455, t.ReferenceA + 1); Invalidate(); } });
        W.Add(new SwitchWidget { Bounds = new RectangleF(cx + 110, by + 2, 140, 28), Text = "MUTE OUTPUT", Get = () => t.MuteOutput, Click = () => t.MuteOutput = !t.MuteOutput, LedColor = Color.FromArgb(255, 80, 60) });
        W.Add(new ButtonWidget { Bounds = new RectangleF(cx - 60, by + 52, 120, 34), Text = "CLOSE TUNER", Click = (_, _) => CloseRequested?.Invoke() });
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        Gfx.Cover(g, Assets.Image("backdrop.jpg"), r);
        using (var dark = new SolidBrush(Color.FromArgb(205, 6, 5, 4))) g.FillRectangle(dark, r);
        Gfx.Vignette(g, r, 200);
        using var hf = new Font("Segoe UI", 15f, FontStyle.Bold);
        using var gold = new SolidBrush(Color.FromArgb(220, Gfx.Gold));
        g.DrawString("TUNER", hf, gold, 28, 18, Gfx.Typo);
        using var sf = new Font("Segoe UI", 9f);
        using var dim = new SolidBrush(Color.FromArgb(160, 230, 220, 200));
        g.DrawString("Play a single string and let it ring.  T or Esc closes the tuner.", sf, dim, 31, 48, Gfx.Typo);

        // meter scale
        using var tick = new Pen(Color.FromArgb(120, 230, 220, 200), 1.5f);
        using var small = new Font("Segoe UI", 8f);
        for (int c = -50; c <= 50; c += 5)
        {
            float x = meter.X + (c + 50) / 100f * meter.Width;
            float h = c % 25 == 0 ? 22 : c % 10 == 0 ? 15 : 9;
            g.DrawLine(tick, x, meter.Bottom - 22, x, meter.Bottom - 22 - h);
            if (c % 25 == 0) g.DrawString((c > 0 ? "+" : "") + c, small, dim, new RectangleF(x - 20, meter.Bottom - 18, 40, 16), Gfx.Center);
        }
        // in-tune zone
        float zx = meter.X + (47 / 100f) * meter.Width, zw = meter.Width * 0.06f;
        using (var zone = new SolidBrush(Color.FromArgb(50, InTune))) g.FillRectangle(zone, zx, meter.Y, zw, meter.Height - 22);
        using var big = new Font("Segoe UI", 22f, FontStyle.Bold);
        Gfx.DrawText(g, "♭", big, dim, new RectangleF(meter.X - 44, meter.Y + 4, 60, 0), 0, 0);
        Gfx.DrawText(g, "♯", big, dim, new RectangleF(meter.Right + 12, meter.Y + 4, 60, 0), 0, 0);

        // strobe frame
        using var sp = Gfx.Round(strobe, 6);
        using (var sb = new SolidBrush(Color.FromArgb(18, 16, 14))) g.FillPath(sb, sp);
        using (var pen = new Pen(Color.FromArgb(80, Gfx.Gold))) g.DrawPath(pen, sp);
    }

    protected override void PaintOverlay(Graphics g)
    {
        bool sig = PowerOn() && t.HasSignal;
        var (note, oct, cents) = sig ? t.Describe(t.Frequency) : ("–", 0, 0.0);
        double ac = Math.Abs(cents);
        Color col = !sig ? Color.FromArgb(90, 230, 220, 200) : ac < 3 ? InTune : ac < 15 ? Off : Far;

        // note readout
        using (var nf = new Font("Segoe UI", 120f, FontStyle.Bold, GraphicsUnit.Pixel))
        using (var nb = new SolidBrush(col))
        {
            var sz = g.MeasureString(note, nf, PointF.Empty, Gfx.Typo);
            float nx = readout.X + readout.Width / 2 - sz.Width / 2;
            if (sig && ac < 3)
            {
                using var gp = new GraphicsPath(); var gr = new RectangleF(readout.X + readout.Width / 2 - 170, readout.Y - 10, 340, 200); gp.AddEllipse(gr);
                using var glow = new PathGradientBrush(gp) { CenterColor = Color.FromArgb(60, InTune), SurroundColors = new[] { Color.FromArgb(0, InTune) } };
                g.FillEllipse(glow, gr);
            }
            g.DrawString(note, nf, nb, nx, readout.Y, Gfx.Typo);
            if (sig)
                using (var of = new Font("Segoe UI", 34f, FontStyle.Bold, GraphicsUnit.Pixel))
                    g.DrawString(oct.ToString(), of, nb, nx + sz.Width - 4, readout.Y + 100, Gfx.Typo);
        }
        using (var info = new Font("Segoe UI", 13f, FontStyle.Bold))
        using (var ib = new SolidBrush(Gfx.Text))
        {
            string s = !PowerOn() ? "Power is off — turn the amp on to tune"
                     : sig ? $"{t.Frequency:0.00} Hz      {(cents >= 0 ? "+" : "")}{cents:0.0} ¢" : "Waiting for a note…";
            g.DrawString(s, info, ib, new RectangleF(readout.X, readout.Bottom - 50, readout.Width, 30), Gfx.Center);
        }

        // nearest standard-tuning string
        using (var stf = new Font("Segoe UI", 9.5f, FontStyle.Bold))
        {
            double midi = sig ? 69 + 12 * Math.Log2(t.Frequency / t.ReferenceA) : -100;
            int nearest = -1; double best = 1.5;
            for (int i = 0; i < Strings.Length; i++) if (Math.Abs(midi - Strings[i].Midi) < best) { best = Math.Abs(midi - Strings[i].Midi); nearest = i; }
            float sw = 58, sx = readout.X + readout.Width / 2 - sw * Strings.Length / 2;
            for (int i = 0; i < Strings.Length; i++)
            {
                var b = new RectangleF(sx + i * sw + 4, readout.Bottom - 12, sw - 8, 24);
                using var bp = Gfx.Round(b, 5);
                bool on = i == nearest;
                using (var bb = new SolidBrush(on ? Color.FromArgb(80, col) : Color.FromArgb(30, 255, 255, 255))) g.FillPath(bb, bp);
                using var tb = new SolidBrush(on ? col : Gfx.Dim);
                g.DrawString(Strings[i].Name, stf, tb, b, Gfx.Center);
            }
        }

        // needle (eased toward the target)
        double target = sig ? Math.Clamp(cents, -50, 50) : 0;
        needle += (target - needle) * 0.35;
        float nx2 = meter.X + (float)((needle + 50) / 100) * meter.Width;
        using (var np = new Pen(sig ? col : Color.FromArgb(60, 230, 220, 200), 4) { StartCap = LineCap.Round, EndCap = LineCap.Round })
            g.DrawLine(np, nx2, meter.Y, nx2, meter.Bottom - 24);
        Gfx.Led(g, new RectangleF(meter.X - 30, meter.Y + 50, 10, 10), sig && cents < -3, Off);
        Gfx.Led(g, new RectangleF(meter.Right + 20, meter.Y + 50, 10, 10), sig && cents > 3, Off);

        // strobe: stripes drift left when flat, right when sharp, stand still in tune
        long now = Environment.TickCount64; double dt = (now - lastTick) / 1000.0; lastTick = now;
        if (sig) strobePhase += cents * 2.2 * dt;
        var st = g.Save();
        using (var sp = Gfx.Round(RectangleF.Inflate(strobe, -3, -3), 4)) g.SetClip(sp);
        float period = 28;
        double off = (strobePhase * period) % period; if (off < 0) off += period;
        using (var sb = new SolidBrush(sig ? Color.FromArgb(200, col) : Color.FromArgb(40, 230, 220, 200)))
            for (float x = strobe.X - period + (float)off; x < strobe.Right; x += period)
                g.FillRectangle(sb, x, strobe.Y + 4, period / 2, strobe.Height - 8);
        g.Restore(st);
    }

    public override void Tick()
    {
        var e = Engine();
        if (e != null && PowerOn()) t.Analyze(e.SampleRate);
        InvalidateArea(RectangleF.Union(readout, strobe));
        InvalidateArea(new RectangleF(meter.X - 50, meter.Y - 10, meter.Width + 100, meter.Height + 20));
    }
}
