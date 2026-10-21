using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.IO;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>Cab page: speaker with a draggable mic, cab model selection, IR loader and mic EQ.</summary>
sealed class CabPage : WidgetSurface
{
    readonly RigModel m;
    Rectangle box, panel;
    PointF spk; float spkR;
    public event Action CabChanged, LoadIrRequested;

    public CabPage(RigModel m)
    {
        this.m = m; BackColor = Color.Black;
        ParamsChanged += _ => InvalidateArea(Rectangle.Inflate(box, 90, 90)); // mic size/position follow the knobs
    }

    protected override void Arrange(Rectangle r)
    {
        int size = Math.Min(r.Height - 70, (int)(r.Width * 0.5));
        int pw = 380, total = size + 50 + pw;
        int x0 = r.X + (r.Width - total) / 2, y0 = r.Y + (r.Height - size) / 2;
        box = new Rectangle(x0, y0, size, size);
        panel = new Rectangle(box.Right + 50, y0, pw, size);
        spk = new PointF(box.X + size / 2f, box.Y + size / 2f);
        spkR = size * 0.38f;

        // the mic: drag across the speaker from the dust cap (centre) to the cone edge
        W.Add(new CustomWidget
        {
            OnPaint = (g, b, hot, pressed) => PaintMic(g, hot || pressed),
            OnDown = (e, s) => MoveMic(e.X),
            OnDrag = (e, s) => MoveMic(e.X),
        });
        UpdateMicBounds();

        var p = panel;
        W.Add(new SwitchWidget { Bounds = new RectangleF(p.Right - 104, p.Y + 22, 84, 28), Text = "CAB ON", Get = () => m.Cab.Enabled, Click = () => m.Cab.Enabled = !m.Cab.Enabled, LedColor = Color.FromArgb(80, 180, 255) });
        string[] models = { Convolver.Models[0], Convolver.Models[1], Convolver.Models[2], "USER IR" };
        for (int i = 0; i < 4; i++)
        {
            int idx = i;
            W.Add(new ButtonWidget
            {
                Bounds = new RectangleF(p.X + 20 + (i % 2) * 172, p.Y + 86 + (i / 2) * 40, 164, 32), Text = models[i], FontSize = 8.5f,
                Active = () => m.Cab.Model == idx,
                Click = (_, _) =>
                {
                    if (idx == 3 && m.Cab.IrData == null) { LoadIrRequested?.Invoke(); return; }
                    m.Cab.Model = idx; CabChanged?.Invoke(); Invalidate();
                },
            });
        }
        W.Add(new ButtonWidget { Bounds = new RectangleF(p.X + 20, p.Y + 176, 120, 30), Text = "LOAD IR…", Click = (_, _) => LoadIrRequested?.Invoke() });
        W.Add(new CustomWidget
        {
            Bounds = new RectangleF(p.X + 150, p.Y + 176, p.Width - 170, 30), Hit = RectangleF.Empty,
            OnPaint = (g, b, _, _) =>
            {
                using var f = new Font("Segoe UI", 8.5f);
                using var tb = new SolidBrush(Gfx.Dim);
                string name = m.Cab.IrPath == null ? "no IR loaded" : Path.GetFileName(m.Cab.IrPath);
                g.DrawString(name, f, tb, b, Gfx.Format(StringAlignment.Near, StringAlignment.Center, true, StringTrimming.EllipsisCharacter));
            },
        });

        var knobs = m.Cab.Knobs;
        float ks = 66, gx = (p.Width - 40) / 2f;
        for (int i = 0; i < knobs.Length; i++)
        {
            float cx = p.X + 20 + gx * (i % 2) + gx / 2, cy = p.Y + 322 + (i / 2) * 124;
            if (cy + ks / 2 > p.Bottom - 30) cy = p.Bottom - 30 - ks / 2;
            var kw = new KnobWidget(knobs[i], Assets.Image("knob.png")) { Bounds = new RectangleF(cx - ks / 2, cy - ks / 2, ks, ks), Ticks = true, TickColor = Color.FromArgb(150, 214, 178, 96) };
            W.Add(kw);
        }
    }

    void MoveMic(float x)
    {
        m.Cab.MicPos.Value = Math.Clamp((x - spk.X) / (spkR * 0.92f), 0, 1);
        InvalidateArea(RectangleF.Inflate(new RectangleF(spk.X - spkR, spk.Y - spkR, spkR * 2, spkR * 2), 60, 60));
        UpdateMicBounds();
    }

    float MicSize => spkR * (0.42f - 0.16f * m.Cab.Distance.Value);

    void UpdateMicBounds()
    {
        var mic = (CustomWidget)W[0];
        float ms = MicSize, mx = spk.X + m.Cab.MicPos.Value * spkR * 0.92f;
        mic.Bounds = new RectangleF(mx - ms / 2, spk.Y - ms / 2, ms, ms);
        mic.Hit = RectangleF.Inflate(new RectangleF(spk.X - spkR, spk.Y - ms / 2, spkR * 2, ms), 0, 10);
    }

    void PaintMic(Graphics g, bool hot)
    {
        UpdateMicBounds();
        var b = W[0].Bounds;
        // stand arm coming in from the lower right
        var armEnd = new PointF(box.Right - 6, box.Bottom - 6);
        using (var arm = new Pen(Color.FromArgb(90, 0, 0, 0), b.Width * 0.16f + 4) { EndCap = LineCap.Round })
            g.DrawLine(arm, b.X + b.Width / 2 + 5, b.Y + b.Height / 2 + 8, armEnd.X + 5, armEnd.Y + 8);
        using (var arm = new LinearGradientBrush(new PointF(b.X, b.Y), armEnd, Color.FromArgb(60, 60, 64), Color.FromArgb(18, 18, 20)))
        using (var armPen = new Pen(arm, b.Width * 0.16f) { EndCap = LineCap.Round })
            g.DrawLine(armPen, b.X + b.Width / 2, b.Y + b.Height / 2, armEnd.X, armEnd.Y);
        using (var sh = new SolidBrush(Color.FromArgb(120, 0, 0, 0))) g.FillEllipse(sh, b.X + 6, b.Y + 10, b.Width, b.Height);
        g.DrawImage(Assets.Image("mic.png"), b);
        if (hot) using (var pen = new Pen(Color.FromArgb(180, Gfx.Gold), 2)) g.DrawEllipse(pen, RectangleF.Inflate(b, 4, 4));
        using var f = new Font("Segoe UI", 8.5f, FontStyle.Bold);
        using var tb = new SolidBrush(Gfx.Gold);
        g.DrawString(m.Cab.MicPos.Display + " · " + m.Cab.Distance.Display, f, tb, new RectangleF(box.X, box.Bottom - 30, box.Width, 20), Gfx.Center);
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        Gfx.Cover(g, Assets.Image("backdrop.jpg"), r);
        using (var dark = new SolidBrush(Color.FromArgb(140, 0, 0, 0))) g.FillRectangle(dark, r);
        Gfx.Vignette(g, r, 200);

        // cabinet with the grille cloth removed so the speaker is visible
        Gfx.Shadow(g, box, 16, 170);
        using (var path = Gfx.Round(box, 16))
        {
            var st = g.Save(); g.SetClip(path);
            Gfx.Tile(g, Assets.Image("tolex.jpg"), box, 0.5f);
            g.Restore(st);
        }
        var baffle = Rectangle.Inflate(box, -28, -28);
        using (var bb = new LinearGradientBrush(baffle, Color.FromArgb(28, 26, 24), Color.FromArgb(10, 9, 8), 90f)) g.FillRectangle(bb, baffle);
        using (var pen = new Pen(Gfx.Gold, 2)) g.DrawRectangle(pen, baffle);
        var sr = new RectangleF(spk.X - spkR * 1.12f, spk.Y - spkR * 1.12f, spkR * 2.24f, spkR * 2.24f);
        using (var sh = new SolidBrush(Color.FromArgb(160, 0, 0, 0))) g.FillEllipse(sh, RectangleF.Inflate(sr, 6, 6));
        g.DrawImage(Assets.Image("speaker.png"), sr);
        Gfx.Logo(g, new RectangleF(baffle.X + 10, baffle.Y + 4, 260, 44), 16, StringAlignment.Near);

        // control panel
        Gfx.Shadow(g, panel, 12, 140);
        using (var pp = Gfx.Round(panel, 12))
        {
            using var pb = new LinearGradientBrush(panel, Color.FromArgb(235, 36, 32, 28), Color.FromArgb(235, 18, 16, 14), 90f);
            g.FillPath(pb, pp);
            using var pen = new Pen(Color.FromArgb(90, Gfx.Gold), 1); g.DrawPath(pen, pp);
        }
        using var tf = new Font("Segoe UI", 14f, FontStyle.Bold);
        using var gold = new SolidBrush(Gfx.Gold);
        g.DrawString("CABINET", tf, gold, panel.X + 18, panel.Y + 20, Gfx.Typo);
        using var sf = new Font("Segoe UI", 8f, FontStyle.Bold);
        using var dim = new SolidBrush(Gfx.Dim);
        g.DrawString("SPEAKER MODEL", sf, dim, panel.X + 22, panel.Y + 64, Gfx.Typo);
        g.DrawString("MICROPHONE — drag the mic across the speaker", sf, dim, panel.X + 22, panel.Y + 226, Gfx.Typo);
        using var lf = new Font("Arial", 7.5f, FontStyle.Bold);
        using var lb = new SolidBrush(Gfx.Text);
        foreach (var w in W)
            if (w is KnobWidget k)
                g.DrawString(k.P.Name, lf, lb, new RectangleF(k.Bounds.X - 30, k.Bounds.Y - 26, k.Bounds.Width + 60, 14), Gfx.CenterNoWrap);
    }
}
