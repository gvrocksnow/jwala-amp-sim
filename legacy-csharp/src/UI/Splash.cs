using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;

namespace Jwala.UI;

/// <summary>
/// Startup splash: a small borderless card with the Jwala logo and the version, shown on its own
/// thread while <see cref="MainForm"/> loads its art. The main window stays DWM-cloaked (invisible,
/// but painting normally) until it has drawn, then is revealed and the splash fades out.
/// Same look and behaviour as the Rust standalone (rust/src/splash.rs, rust/src/reveal.rs).
/// </summary>
static class Splash
{
    static volatile bool ready;
    static IntPtr cloaked;

    /// <summary>Shows the splash on a background thread; it closes itself once <see cref="Reveal"/> runs.</summary>
    public static void Show()
    {
        var t = new Thread(() =>
        {
            // a splash that fails to draw is never worth breaking startup over
            try { Application.Run(new SplashForm()); } catch { }
            RevealWindow();
        }) { IsBackground = true, Name = "Splash" };
        t.SetApartmentState(ApartmentState.STA);
        t.Start();
    }

    /// <summary>Keeps <paramref name="form"/> invisible from the moment its window exists until <see cref="Reveal"/>.</summary>
    public static void HideUntilReady(Form form)
    {
        void Cloak(object _, EventArgs __)
        {
            form.HandleCreated -= Cloak;
            if (SetCloak(form.Handle, true)) Interlocked.Exchange(ref cloaked, form.Handle);
        }
        if (form.IsHandleCreated) Cloak(null, null); else form.HandleCreated += Cloak;
        // after the form's own Shown work (it starts audio there): paint everything, then show it
        form.Shown += (_, _) =>
        {
            form.Refresh();
            Reveal();
            form.Activate();
        };
    }

    /// <summary>The main window has drawn: uncloak it and let the splash fade out.</summary>
    public static void Reveal()
    {
        RevealWindow();
        ready = true;
    }

    static void RevealWindow()
    {
        var h = Interlocked.Exchange(ref cloaked, IntPtr.Zero);
        if (h != IntPtr.Zero) SetCloak(h, false);
    }

    static bool SetCloak(IntPtr hwnd, bool on)
    {
        int v = on ? 1 : 0;
        return DwmSetWindowAttribute(hwnd, 13 /* DWMWA_CLOAK */, ref v, 4) >= 0;
    }

    [DllImport("dwmapi.dll")] static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    sealed class SplashForm : Form
    {
        // card size and margin for its shadow, in logical px
        const float CardW = 300, CardH = 280, Radius = 18, ShadowW = 22, LogoSize = 132;
        const double FadeIn = 0.12, FadeOut = 0.22, TimeoutSec = 20;

        readonly Bitmap card;
        readonly System.Windows.Forms.Timer timer = new() { Interval = 15 };
        readonly DateTime start = DateTime.UtcNow;
        DateTime? closing;

        public SplashForm()
        {
            FormBorderStyle = FormBorderStyle.None;
            ShowInTaskbar = false;
            StartPosition = FormStartPosition.Manual;
            Text = "Jwala";
            float scale;
            using (var g = Graphics.FromHwnd(IntPtr.Zero)) scale = g.DpiX / 96f;
            card = Compose(scale);
            var area = Screen.PrimaryScreen.WorkingArea;
            Bounds = new Rectangle(area.X + (area.Width - card.Width) / 2, area.Y + (area.Height - card.Height) / 2, card.Width, card.Height);
            timer.Tick += (_, _) => Tick();
        }

        protected override bool ShowWithoutActivation => true;

        protected override CreateParams CreateParams
        {
            get
            {
                var cp = base.CreateParams;
                // WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
                cp.ExStyle |= 0x0008_0000 | 0x0000_0080 | 0x0000_0008 | 0x0800_0000;
                return cp;
            }
        }

        protected override void OnHandleCreated(EventArgs e)
        {
            base.OnHandleCreated(e);
            Present(0);
            timer.Start();
        }

        void Tick()
        {
            double t = (DateTime.UtcNow - start).TotalSeconds;
            if (closing == null && (ready || t > TimeoutSec)) closing = DateTime.UtcNow;
            double alpha = Math.Min(1, t / FadeIn);
            if (closing is DateTime c)
            {
                double o = (DateTime.UtcNow - c).TotalSeconds / FadeOut;
                if (o >= 1) { timer.Stop(); Close(); return; }
                alpha *= 1 - o;
            }
            Present(alpha);
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) { timer.Dispose(); card.Dispose(); }
            base.Dispose(disposing);
        }

        // ---- per-pixel-alpha presentation (UpdateLayeredWindow) ----

        void Present(double alpha)
        {
            IntPtr screen = GetDC(IntPtr.Zero), mem = CreateCompatibleDC(screen);
            IntPtr bmp = card.GetHbitmap(Color.FromArgb(0)), old = SelectObject(mem, bmp);
            try
            {
                var size = new SIZE { cx = card.Width, cy = card.Height };
                var src = new POINT();
                var dst = new POINT { x = Left, y = Top };
                var blend = new BLENDFUNCTION { SourceConstantAlpha = (byte)(Math.Clamp(alpha, 0, 1) * 255), AlphaFormat = 1 /* AC_SRC_ALPHA */ };
                UpdateLayeredWindow(Handle, screen, ref dst, ref size, mem, ref src, 0, ref blend, 2 /* ULW_ALPHA */);
            }
            finally
            {
                SelectObject(mem, old);
                DeleteObject(bmp);
                DeleteDC(mem);
                ReleaseDC(IntPtr.Zero, screen);
            }
        }

        // ---- drawing ----

        /// <summary>Signed distance from (px, py) to a rounded rectangle centred at (cx, cy) (negative inside).</summary>
        static float RRectSdf(float px, float py, float cx, float cy, float hw, float hh, float r)
        {
            float qx = Math.Abs(px - cx) - (hw - r), qy = Math.Abs(py - cy) - (hh - r);
            float ox = Math.Max(qx, 0), oy = Math.Max(qy, 0);
            return MathF.Sqrt(ox * ox + oy * oy) + Math.Min(Math.Max(qx, qy), 0) - r;
        }

        static GraphicsPath RoundRect(RectangleF r, float radius)
        {
            float d = radius * 2;
            var p = new GraphicsPath();
            p.AddArc(r.X, r.Y, d, d, 180, 90);
            p.AddArc(r.Right - d, r.Y, d, d, 270, 90);
            p.AddArc(r.Right - d, r.Bottom - d, d, d, 0, 90);
            p.AddArc(r.X, r.Bottom - d, d, d, 90, 90);
            p.CloseFigure();
            return p;
        }

        /// <summary>The card at <paramref name="scale"/> as a straight-alpha ARGB bitmap.</summary>
        static Bitmap Compose(float scale)
        {
            float S(float v) => v * scale;
            int w = (int)MathF.Round(S(CardW + 2 * ShadowW)), h = (int)MathF.Round(S(CardH + 2 * ShadowW));
            float cx = w / 2f, cy = h / 2f, hw = S(CardW) / 2, hh = S(CardH) / 2, r = S(Radius), top = cy - hh;

            // shadow and card gradient, composed per pixel (premultiplied) like the Rust splash
            var px = new int[w * h];
            for (int y = 0; y < h; y++)
                for (int x = 0; x < w; x++)
                {
                    float fx = x + 0.5f, fy = y + 0.5f, pr = 0, pg = 0, pb = 0, pa = 0;
                    void Blend(float rr, float gg, float bb, float a)
                    {
                        if (a <= 0) return;
                        float k = 1 - a;
                        pr = rr * a + pr * k; pg = gg * a + pg * k; pb = bb * a + pb * k; pa = a + pa * k;
                    }
                    float ds = RRectSdf(fx, fy - S(5), cx, cy, hw, hh, r);
                    float sh = 1 - Math.Clamp(ds / S(ShadowW), 0, 1);
                    Blend(0, 0, 0, 0.45f * sh * sh);
                    float d = RRectSdf(fx, fy, cx, cy, hw, hh, r);
                    float cover = Math.Clamp(0.5f - d, 0, 1);
                    if (cover > 0)
                    {
                        float t = Math.Clamp((fy - top) / (2 * hh), 0, 1);
                        Blend(Lerp(0x22, 0x0f, t), Lerp(0x22, 0x0f, t), Lerp(0x26, 0x11, t), cover);
                        float rim = Math.Clamp(1 - Math.Abs(d + S(1)) / S(1), 0, 1);
                        Blend(1, 1, 1, 0.07f * rim);
                    }
                    // stored straight (un-premultiplied) for a 32bppArgb bitmap
                    float ia = pa > 0 ? 1 / pa : 0;
                    px[y * w + x] = (Q(pa) << 24) | (Q(pr * ia) << 16) | (Q(pg * ia) << 8) | Q(pb * ia);
                }
            var bmp = new Bitmap(w, h, PixelFormat.Format32bppArgb);
            var data = bmp.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
            for (int y = 0; y < h; y++) Marshal.Copy(px, y * w, data.Scan0 + y * data.Stride, w);
            bmp.UnlockBits(data);

            using var g = Graphics.FromImage(bmp);
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.InterpolationMode = InterpolationMode.HighQualityBicubic;
            g.PixelOffsetMode = PixelOffsetMode.HighQuality;
            g.TextRenderingHint = TextRenderingHint.AntiAlias;

            // the logo, clipped (anti-aliased) to the rounded white frame drawn inside the opaque icon
            int size = (int)MathF.Round(S(LogoSize));
            float lx = MathF.Round(cx - size / 2f), ly = MathF.Round(top + S(34));
            using (var res = typeof(Splash).Assembly.GetManifestResourceStream("jwala_512.png"))
            using (var src = new Bitmap(res))
            using (var logo = new Bitmap(size, size, PixelFormat.Format32bppArgb))
            {
                using (var lg = Graphics.FromImage(logo))
                {
                    lg.InterpolationMode = InterpolationMode.HighQualityBicubic;
                    lg.PixelOffsetMode = PixelOffsetMode.HighQuality;
                    lg.DrawImage(src, 0, 0, size, size);
                }
                float inset = size * 0.044f;
                using var brush = new TextureBrush(logo) { WrapMode = WrapMode.Clamp };
                brush.TranslateTransform(lx, ly);
                using var path = RoundRect(new RectangleF(lx + inset, ly + inset, size - 2 * inset, size - 2 * inset), size * 0.085f);
                g.FillPath(brush, path);
            }

            // name and version, baselines matching the Rust splash
            float below = ly + size;
            // the pre-rendered "Jwala" (assets/logo_splash.png, a white mask; geometry in rust/src/logo.rs),
            // its pen origin where the 34 px text starts
            using (var res = typeof(Splash).Assembly.GetManifestResourceStream("logo_splash.png"))
            using (var name = new Bitmap(res))
            using (var tint = new ImageAttributes())
            {
                const float W = 118, H = 59, OriginX = 2, OriginY = 40, Advance = 111.01f;
                tint.SetColorMatrix(new ColorMatrix { Matrix00 = 0xf2 / 255f, Matrix11 = 0xf2 / 255f, Matrix22 = 0xf2 / 255f });
                float nx = MathF.Round(cx - S(Advance) / 2) - MathF.Round(S(OriginX)), ny = MathF.Round(below + S(48) - S(OriginY));
                var dest = Rectangle.Round(new RectangleF(nx, ny, S(W), S(H)));
                var mode = g.InterpolationMode;
                g.InterpolationMode = InterpolationMode.HighQualityBicubic;
                g.DrawImage(name, dest, 0, 0, name.Width, name.Height, GraphicsUnit.Pixel, tint);
                g.InterpolationMode = mode;
            }
            var v = typeof(Splash).Assembly.GetName().Version;
            DrawCentered(g, $"v{v.Major}.{v.Minor}.{v.Build}", "Segoe UI", FontStyle.Regular, S(13), cx, below + S(78), Color.FromArgb(0x8a, 0x8a, 0x90));
            return bmp;

            static float Lerp(int a, int b, float t) => (a + (b - a) * t) / 255f;
            static int Q(float v) => (int)(Math.Clamp(v, 0, 1) * 255 + 0.5f);
        }

        /// <summary>Draws <paramref name="text"/> centred on <paramref name="cx"/> with its baseline at <paramref name="baseline"/>.</summary>
        static void DrawCentered(Graphics g, string text, string family, FontStyle style, float px, float cx, float baseline, Color color)
        {
            using var font = Fonts.Get(family, px, style, GraphicsUnit.Pixel);
            var ff = font.FontFamily;
            float ascent = px * ff.GetCellAscent(style) / ff.GetEmHeight(style);
            using var brush = new SolidBrush(color);
            using var fmt = new StringFormat(StringFormat.GenericTypographic) { Alignment = StringAlignment.Center };
            g.DrawString(text, font, brush, cx, baseline - ascent, fmt);
        }

        [StructLayout(LayoutKind.Sequential)] struct POINT { public int x, y; }
        [StructLayout(LayoutKind.Sequential)] struct SIZE { public int cx, cy; }
        [StructLayout(LayoutKind.Sequential, Pack = 1)] struct BLENDFUNCTION { public byte BlendOp, BlendFlags, SourceConstantAlpha, AlphaFormat; }

        [DllImport("user32.dll")] static extern bool UpdateLayeredWindow(IntPtr hwnd, IntPtr hdcDst, ref POINT pptDst, ref SIZE psize, IntPtr hdcSrc, ref POINT pptSrc, int crKey, ref BLENDFUNCTION pblend, int dwFlags);
        [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
        [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr dc);
        [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
        [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
        [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);
        [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    }
}
