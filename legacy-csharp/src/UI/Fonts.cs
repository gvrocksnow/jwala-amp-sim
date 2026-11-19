using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Text;
using System.Runtime.InteropServices;

namespace Jwala.UI;

/// <summary>
/// The UI's typefaces, embedded so the app doesn't depend on the system's fonts (the same set as the
/// Rust version, see rust/src/fonts.rs; licences in assets/fonts/). Code asks for the Windows fonts the
/// UI was designed around and gets their open-licensed stand-ins:
/// Segoe UI (and its Semibold / Light) → Selawik, Arial → Arimo Bold, Segoe UI Symbol → DejaVu Sans.
/// Their GDI+ cell metrics equal the originals', so layouts don't move. The Segoe Script logo is an
/// image (logo_header.png / logo_splash.png).
/// </summary>
static class Fonts
{
    /// <summary>The file behind each requested Windows font, regular and bold. A bold request goes to the
    /// bold file's own family: with both weights in one family, GDI+ draws "Bold" Selawik with the
    /// regular file's glyphs.</summary>
    static readonly Dictionary<string, (string Regular, string Bold)> Substitute = new(StringComparer.OrdinalIgnoreCase)
    {
        ["Segoe UI"] = ("Selawik-Regular.ttf", "Selawik-Bold.ttf"),
        ["Segoe UI Semibold"] = ("Selawik-Semibold.ttf", "Selawik-Bold.ttf"),
        ["Segoe UI Light"] = ("Selawik-Light.ttf", "Selawik-Regular.ttf"),
        ["Arial"] = ("Arimo-Bold.ttf", "Arimo-Bold.ttf"),
        ["Segoe UI Symbol"] = ("DejaVuSans.ttf", "DejaVuSans.ttf"),
    };

    static readonly string[] Files = { "Selawik-Regular.ttf", "Selawik-Bold.ttf", "Selawik-Semibold.ttf", "Selawik-Light.ttf", "Arimo-Bold.ttf", "DejaVuSans.ttf" };

    /// <summary>One collection per file: sharing one, GDI+ mixes up faces of the same family name.</summary>
    static readonly Dictionary<string, FontFamily> families = new(StringComparer.OrdinalIgnoreCase);
    /// <summary>Kept alive with the families: a collection's finalizer would free their fonts.</summary>
    static readonly List<PrivateFontCollection> collections = new();

    static Fonts()
    {
        foreach (var file in Files)
        {
            var collection = new PrivateFontCollection();
            collections.Add(collection);
            using var s = typeof(Fonts).Assembly.GetManifestResourceStream(file)
                          ?? throw new InvalidOperationException("missing font " + file);
            var data = new byte[s.Length];
            s.ReadExactly(data);
            // GDI+ reads the font from this memory for as long as the process runs: never freed
            IntPtr mem = Marshal.AllocCoTaskMem(data.Length);
            Marshal.Copy(data, 0, mem, data.Length);
            collection.AddMemoryFont(mem, data.Length);
            // and GDI too, for the WinForms controls (menus, dialogs), which draw with GDI
            uint installed = 0;
            AddFontMemResourceEx(mem, (uint)data.Length, IntPtr.Zero, ref installed);
            families[file] = collection.Families[0];
        }
    }

    /// <summary>The embedded stand-in for <paramref name="family"/> (a Windows font name), like <c>new Font(family, …)</c>.</summary>
    public static Font Get(string family, float size, FontStyle style = FontStyle.Regular, GraphicsUnit unit = GraphicsUnit.Point)
    {
        if (!Substitute.TryGetValue(family, out var files)) return new Font(family, size, style, unit);
        // the style is kept: GDI+ adds no weight to a bold file, and WinForms controls (drawn by GDI) pick
        // their face by name and the bold flag
        return new Font(families[style.HasFlag(FontStyle.Bold) ? files.Bold : files.Regular], size, style, unit);
    }

    [DllImport("gdi32.dll")] static extern IntPtr AddFontMemResourceEx(IntPtr pbFont, uint cbFont, IntPtr pdv, ref uint pcFonts);
}
