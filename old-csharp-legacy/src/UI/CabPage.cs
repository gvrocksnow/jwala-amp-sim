using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.IO;
using Jwala.Dsp;

namespace Jwala.UI;

/// <summary>Cab page: 4x12 with a draggable mic on a stand, cab model selection, IR loader and mic EQ.</summary>
sealed class CabPage : WidgetSurface
{
    static readonly Color Slate = Color.FromArgb(42, 45, 50);

    // Speaker centres and cone radius as fractions of cab_412.png, and the mic's head centre and tail (where the stand
    // clamps on) as fractions of cab_mic.png (measured by tools/cut_cab.py on concepts/cab412_A2mic_1_clone.png).
    static readonly PointF[] Spk = { new(0.2691f, 0.2993f), new(0.7241f, 0.2993f), new(0.2669f, 0.7122f), new(0.7218f, 0.7122f) };
    const float SpkR = 0.1363f;
    const int MicSpk = 1;                                 // the miked speaker: top right, as in the concept
    static readonly PointF MicHead = new(0.19f, 0.5f), MicTail = new(0.97f, 0.55f);
    const float MicW = 0.195f;                            // mic width relative to the cab at DISTANCE 0

    readonly RigModel m;
    Rectangle box, panel;
    PointF spk; float spkR;
    RectangleF micRect;
    Bitmap micShadow; Size micShadowFor;
    public event Action CabChanged, LoadIrRequested;

    public CabPage(RigModel m)
    {
        this.m = m; BackColor = Slate;
        ParamsChanged += _ => InvalidateArea(W.Count > 0 ? W[0].Dirty : RectangleF.Empty);   // mic size/position follow the knobs
    }

    protected override void Arrange(Rectangle r)
    {
        var cabImg = Assets.Image("cab_412.png");
        // room for the title above, the stand's feet and the mic readout below, and the stand between cab and panel
        int pw = 380, gap = 150;
        int h = Math.Max(300, Math.Min(r.Height - 170, (int)((r.Width - pw - gap - 80) * (float)cabImg.Height / cabImg.Width)));
        int w = (int)(h * (float)cabImg.Width / cabImg.Height);
        int x0 = r.X + (r.Width - (w + gap + pw)) / 2, y0 = r.Y + 96;
        box = new Rectangle(x0, y0, w, h);
        panel = new Rectangle(box.Right + gap, y0, pw, h);
        spk = new PointF(box.X + w * Spk[MicSpk].X, box.Y + h * Spk[MicSpk].Y);
        spkR = w * SpkR;

        // the mic: drag across the speaker from the dust cap (centre) towards the middle of the cab (cone)
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
        var before = W[0].Dirty;
        m.Cab.MicPos.Value = Math.Clamp((spk.X - x) / (spkR * 0.92f), 0, 1);
        UpdateMicBounds();
        InvalidateArea(RectangleF.Union(before, W[0].Dirty));
    }

    /// <summary>The mic image for the current MIC POS and DISTANCE: the head on the speaker, further mics drawn smaller.</summary>
    void UpdateMicBounds()
    {
        var img = Assets.Image("cab_mic.png");
        float mw = box.Width * MicW * (1 - 0.22f * m.Cab.Distance.Value), mh = mw * img.Height / img.Width;
        var head = new PointF(spk.X - m.Cab.MicPos.Value * spkR * 0.92f, spk.Y);
        micRect = new RectangleF(head.X - mw * MicHead.X, head.Y - mh * MicHead.Y, mw, mh);

        // the widget covers the mic, its stand and the readout so a move repaints all of them
        var mic = (CustomWidget)W[0];
        float s = box.Width / 666f;
        var reach = RectangleF.FromLTRB(box.X, micRect.Top, box.Right + 120 * s, box.Bottom + 60);
        mic.Bounds = RectangleF.Union(micRect, reach);
        // grab the mic itself, or anywhere along the speaker's row
        mic.Hit = RectangleF.Union(micRect, new RectangleF(spk.X - spkR, spk.Y - mh / 2 - 10, spkR * 1.3f, mh + 20));
    }

    /// <summary>Boom from the mic's tail down to a knuckle right of the cab, then the pole and tripod on the floor.</summary>
    void PaintStand(Graphics g, RectangleF mic)
    {
        float s = box.Width / 666f;
        var tail = new PointF(mic.X + mic.Width * MicTail.X, mic.Y + mic.Height * MicTail.Y);
        var knuckle = new PointF(box.Right + 56 * s, spk.Y + 210 * s);
        float floor = box.Bottom + 6 * s;
        var pole = new PointF(knuckle.X, floor - 16 * s);
        (float dx, float dy)[] legs = { (-56, 14), (50, 18), (6, 8) };

        // soft shadow first, down and to the right like the cab's
        using (var sh = new Pen(Color.FromArgb(70, 0, 0, 0), 9 * s) { StartCap = LineCap.Round, EndCap = LineCap.Round })
        {
            float ox = 8 * s, oy = 10 * s;
            g.DrawLine(sh, tail.X + ox, tail.Y + oy, knuckle.X + ox, knuckle.Y + oy);
            g.DrawLine(sh, knuckle.X + ox, knuckle.Y + oy, pole.X + ox, pole.Y + oy);
        }
        var dark = Color.FromArgb(20, 20, 23);
        using (var legPen = new Pen(dark, 5 * s) { StartCap = LineCap.Round, EndCap = LineCap.Round })
            foreach (var (dx, dy) in legs) g.DrawLine(legPen, pole.X, pole.Y, pole.X + dx * s, floor + dy * s);
        using (var polePen = new Pen(dark, 7 * s) { StartCap = LineCap.Round, EndCap = LineCap.Round }) g.DrawLine(polePen, knuckle, pole);
        using (var boom = new Pen(dark, 6 * s) { StartCap = LineCap.Round, EndCap = LineCap.Round }) g.DrawLine(boom, tail, knuckle);
        using (var lit = new Pen(Color.FromArgb(34, 255, 255, 255), 1.5f * s))
        {
            g.DrawLine(lit, knuckle.X - 2 * s, knuckle.Y, pole.X - 2 * s, pole.Y);
            g.DrawLine(lit, tail.X, tail.Y - 2 * s, knuckle.X, knuckle.Y - 2 * s);
        }
        using (var db = new SolidBrush(dark))
        {
            g.FillEllipse(db, knuckle.X - 8 * s, knuckle.Y - 8 * s, 16 * s, 16 * s);
            g.FillEllipse(db, tail.X - 5.5f * s, tail.Y - 5.5f * s, 11 * s, 11 * s);
        }
        using (var hl = new SolidBrush(Color.FromArgb(40, 255, 255, 255))) g.FillEllipse(hl, knuckle.X - 5 * s, knuckle.Y - 5 * s, 6 * s, 6 * s);
    }

    /// <summary>The mic's soft shadow, baked once per mic size with the same CPU shadow pass as the page backgrounds.</summary>
    Bitmap MicShadow(Size size, out int pad)
    {
        pad = (int)(size.Height * 1.2f);
        if (micShadow != null && micShadowFor == size) return micShadow;
        micShadow?.Dispose();
        var full = new Size(size.Width + 2 * pad, size.Height + 2 * pad);
        micShadow = new Bitmap(full.Width, full.Height, System.Drawing.Imaging.PixelFormat.Format32bppArgb);
        int p = pad; float md = size.Height;
        using (var g = Graphics.FromImage(micShadow))
            SoftShadows.Cast(g, full, og => og.DrawImage(Assets.Image("cab_mic.png"), new Rectangle(p, p, size.Width, size.Height)), null, new[]
            {
                new SoftShadows.Term(md * 0.06f, md * 0.1f, md * 0.05f, 0.6f),
                new SoftShadows.Term(md * 0.2f, md * 0.3f, md * 0.16f, 0.45f),
                new SoftShadows.Term(md * 0.4f, md * 0.6f, md * 0.4f, 0.3f),
            }, 1);
        micShadowFor = size;
        return micShadow;
    }

    void PaintMic(Graphics g, bool hot)
    {
        UpdateMicBounds();
        var mic = micRect;
        PaintStand(g, mic);
        var shadow = MicShadow(Size.Round(mic.Size), out int pad);
        g.DrawImage(shadow, mic.X - pad, mic.Y - pad, shadow.Width, shadow.Height);
        g.DrawImage(Assets.Image("cab_mic.png"), mic);
        if (hot)
        {
            float d = mic.Height * 1.05f;
            using var pen = new Pen(Color.FromArgb(180, Gfx.Gold), 2);
            g.DrawEllipse(pen, mic.X + mic.Width * MicHead.X - d / 2, mic.Y + mic.Height * MicHead.Y - d / 2, d, d);
        }
        using var f = new Font("Segoe UI", 8.5f, FontStyle.Bold);
        using var tb = new SolidBrush(Gfx.Gold);
        g.DrawString("MIC  " + m.Cab.MicPos.Display + " · " + m.Cab.Distance.Display, f, tb, new RectangleF(box.X, box.Bottom + 40, box.Width, 20), Gfx.Center);
    }

    protected override void PaintStatic(Graphics g)
    {
        var r = View;
        using (var bg = new SolidBrush(Slate)) g.FillRectangle(bg, r);
        using (var tf = new Font("Segoe UI Semibold", 15f))
        using (var tb = new SolidBrush(Color.FromArgb(240, 242, 245)))
            g.DrawString("CABINET", tf, tb, 30, 20, Gfx.Typo);
        using (var sf0 = new Font("Segoe UI", 9f))
        using (var sb = new SolidBrush(Color.FromArgb(150, 205, 210, 220)))
            g.DrawString("Speaker model, mic and room — drag the mic across the speaker (dust cap to cone), or load your own IR", sf0, sb, 33, 52, Gfx.Typo);

        // soft shadow like the amp page, then the 4x12 (grille cloth off)
        var cabImg = Assets.Image("cab_412.png");
        float u = box.Width / 1.6f;
        SoftShadows.Cast(g, r.Size, og => og.DrawImage(cabImg, box), null, new[]
        {
            new SoftShadows.Term(u * 0.012f, u * 0.006f, u * 0.01f, 0.6f),
            new SoftShadows.Term(u * 0.07f, u * 0.035f, u * 0.05f, 0.5f),
            new SoftShadows.Term(u * 0.16f, u * 0.08f, u * 0.16f, 0.4f),
        });
        g.DrawImage(cabImg, box);

        // control panel
        Gfx.Shadow(g, panel, 12, 140);
        using (var pp = Gfx.Round(panel, 12))
        {
            using var pb = new LinearGradientBrush(panel, Color.FromArgb(235, 36, 32, 28), Color.FromArgb(235, 18, 16, 14), 90f);
            g.FillPath(pb, pp);
            using var pen = new Pen(Color.FromArgb(90, Gfx.Gold), 1); g.DrawPath(pen, pp);
        }
        using var tf2 = new Font("Segoe UI", 14f, FontStyle.Bold);
        using var gold = new SolidBrush(Gfx.Gold);
        g.DrawString("CABINET", tf2, gold, panel.X + 18, panel.Y + 20, Gfx.Typo);
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
