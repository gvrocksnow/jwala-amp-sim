using System;

namespace Jwala.Dsp;

static class Fmt
{
    public static string Ms(double ms) => ms >= 1000 ? (ms / 1000).ToString("0.00") + " s" : ms.ToString("0") + " ms";
    public static string Pct(float v) => (v * 100).ToString("0") + "%";
    public static string Hz(double hz) => hz >= 1000 ? (hz / 1000).ToString("0.0") + " kHz" : hz.ToString("0") + " Hz";
}
