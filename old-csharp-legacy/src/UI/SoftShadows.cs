using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

namespace Jwala.UI;

/// <summary>
/// A small CPU shadow "shader", run once per layout and baked into a page's cached background layer.
/// The occluders' alpha is rasterised to a mask, and each light term shifts and blurs it (contact shadow,
/// penumbra, ambient falloff); the terms multiply into a light factor that is applied as a black overlay,
/// optionally only where a receiver's alpha is (e.g. knob shadows fall on the pedal lid only).
/// </summary>
static class SoftShadows
{
    /// <summary>One light term: the shadow is the occluder mask offset by (Dx, Dy), blurred by Blur (~Gaussian
    /// sigma), darkening by up to Strength. All in pixels.</summary>
    public readonly record struct Term(float Dx, float Dy, float Blur, float Strength);

    /// <summary>Draws the shadow cast by whatever <paramref name="occluders"/> paints onto <paramref name="g"/>.
    /// Works at 1/<paramref name="down"/> resolution: the result is smooth, so it is upsampled for free.</summary>
    public static void Cast(Graphics g, Size size, Action<Graphics> occluders, Action<Graphics> receiver, Term[] terms, int down = 2)
    {
        int w = Math.Max(1, size.Width / down), h = Math.Max(1, size.Height / down);
        float[] occ = Mask(w, h, down, occluders);
        float[] rec = receiver == null ? null : Mask(w, h, down, receiver);

        var light = new float[w * h];
        Array.Fill(light, 1f);
        var buf = new float[w * h];
        var tmp = new float[w * h];
        foreach (var t in terms)
        {
            Shift(occ, buf, w, h, (int)MathF.Round(t.Dx / down), (int)MathF.Round(t.Dy / down));
            // three box passes approximate a Gaussian; a box of radius r ~ sigma * 0.95 each
            int r = Math.Max(0, (int)MathF.Round(t.Blur / down * 0.95f));
            for (int pass = 0; pass < 3; pass++) { BoxH(buf, tmp, w, h, r); BoxV(tmp, buf, w, h, r); }
            for (int i = 0; i < light.Length; i++) light[i] *= 1 - t.Strength * Math.Clamp(buf[i], 0, 1);
        }

        var px = new int[w * h];
        for (int i = 0; i < px.Length; i++)
        {
            float a = 1 - light[i];
            if (rec != null) a *= rec[i];
            px[i] = (int)(Math.Clamp(a, 0, 1) * 255) << 24;            // black, shadow in alpha
        }
        using var bmp = new Bitmap(w, h, PixelFormat.Format32bppArgb);
        var bd = bmp.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
        for (int y = 0; y < h; y++) Marshal.Copy(px, y * w, bd.Scan0 + y * bd.Stride, w);
        bmp.UnlockBits(bd);

        using var ia = new ImageAttributes();
        ia.SetWrapMode(WrapMode.TileFlipXY);                          // no dark fringe at the page edge when upsampling
        var im = g.InterpolationMode;
        g.InterpolationMode = InterpolationMode.HighQualityBicubic;
        g.DrawImage(bmp, new Rectangle(0, 0, size.Width, size.Height), 0, 0, w, h, GraphicsUnit.Pixel, ia);
        g.InterpolationMode = im;
    }

    static float[] Mask(int w, int h, int down, Action<Graphics> paint)
    {
        using var bmp = new Bitmap(w, h, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(bmp))
        {
            g.InterpolationMode = InterpolationMode.HighQualityBilinear;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.ScaleTransform(1f / down, 1f / down);
            paint(g);
        }
        var bd = bmp.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        var px = new int[w * h];
        for (int y = 0; y < h; y++) Marshal.Copy(bd.Scan0 + y * bd.Stride, px, y * w, w);
        bmp.UnlockBits(bd);
        var m = new float[w * h];
        for (int i = 0; i < m.Length; i++) m[i] = ((uint)px[i] >> 24) / 255f;
        return m;
    }

    static void Shift(float[] src, float[] dst, int w, int h, int dx, int dy)
    {
        Array.Clear(dst);
        for (int y = 0; y < h; y++)
        {
            int sy = y - dy;
            if (sy < 0 || sy >= h) continue;
            int x0 = Math.Max(0, dx), x1 = Math.Min(w, w + dx);
            if (x1 > x0) Array.Copy(src, sy * w + x0 - dx, dst, y * w + x0, x1 - x0);
        }
    }

    /// <summary>Running-sum box blur along rows (edges clamp).</summary>
    static void BoxH(float[] src, float[] dst, int w, int h, int r)
    {
        if (r == 0) { Array.Copy(src, dst, src.Length); return; }
        float inv = 1f / (2 * r + 1);
        for (int y = 0; y < h; y++)
        {
            int row = y * w;
            float acc = 0;
            for (int k = -r; k <= r; k++) acc += src[row + Math.Clamp(k, 0, w - 1)];
            for (int x = 0; x < w; x++)
            {
                dst[row + x] = acc * inv;
                acc += src[row + Math.Min(x + r + 1, w - 1)] - src[row + Math.Max(x - r, 0)];
            }
        }
    }

    /// <summary>Running-sum box blur along columns (edges clamp).</summary>
    static void BoxV(float[] src, float[] dst, int w, int h, int r)
    {
        if (r == 0) { Array.Copy(src, dst, src.Length); return; }
        float inv = 1f / (2 * r + 1);
        for (int x = 0; x < w; x++)
        {
            float acc = 0;
            for (int k = -r; k <= r; k++) acc += src[Math.Clamp(k, 0, h - 1) * w + x];
            for (int y = 0; y < h; y++)
            {
                dst[y * w + x] = acc * inv;
                acc += src[Math.Min(y + r + 1, h - 1) * w + x] - src[Math.Max(y - r, 0) * w + x];
            }
        }
    }
}
