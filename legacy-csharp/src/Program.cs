using System;
using System.Diagnostics;
using System.Linq;
using System.Windows.Forms;
using NAudio.Wave;
using Jwala.Dsp;

namespace Jwala;

static class Program
{
    [STAThread]
    static int Main(string[] args)
    {
        if (args.Length >= 3 && args[0] == "--render")
            return Render(args);
        ApplicationConfiguration.Initialize();
        if (args.Length >= 2 && args[0] == "--screenshot")
            return Screenshot(args[1], args.Length > 2 ? args[2] : null, args.Length > 3 ? args[3] : null);
        if (args.Length >= 1 && args[0] == "--tunertest") return TunerTest();
        if (args.Length >= 1 && args[0] == "--selftest") return SelfTest(args.Length > 1 ? args[1] : null);
        // --no-audio: dev aid, starts without opening a device (and leaves the saved settings alone)
        Application.Run(new MainForm(autoStart: !(args.Length >= 1 && args[0] == "--no-audio")));
        return 0;
    }

    /// <summary>Dev aid: renders the window (optionally a given page, at an optional WxH client size) to a PNG.</summary>
    static int Screenshot(string path, string page, string size)
    {
        using var f = new MainForm(autoStart: false) { StartPosition = FormStartPosition.Manual, Location = new System.Drawing.Point(-3000, 0) };
        if (size != null && size.Split('x') is [var sw, var sh]) f.ClientSize = new System.Drawing.Size(int.Parse(sw), int.Parse(sh));
        f.Show(); Application.DoEvents();
        if (page == "tuner") f.OpenTunerForScreenshot(); else if (page != null) f.ShowPage(page);
        Application.DoEvents();
        using var bmp = new System.Drawing.Bitmap(f.Width, f.Height);
        f.DrawToBitmap(bmp, new System.Drawing.Rectangle(0, 0, f.Width, f.Height));
        bmp.Save(path);
        return 0;
    }

    /// <summary>Dev aid: feeds synthetic plucked notes through the tuner and prints what it hears.</summary>
    static int TunerTest()
    {
        const int fs = 48000;
        var rnd = new Random(1);
        foreach (double f0 in new[] { 61.74, 82.41, 110.0, 146.83, 196.0, 246.94, 329.63, 440 * Math.Pow(2, 7 / 1200.0), 659.26 })
        {
            var t = new Tuner { Active = true };
            var buf = new float[256];
            double ph = 0; int n = 0; string res = "none";
            for (int blk = 0; blk < 40; blk++) // ~0.2 s
            {
                for (int i = 0; i < buf.Length; i++, n++)
                {
                    ph += f0 / fs;
                    double env = Math.Exp(-n / (fs * 1.5)), v = 0;
                    for (int h = 1; h <= 8; h++) v += Math.Sin(2 * Math.PI * h * ph + h) / (h * (h == 1 ? 0.6 : 1)) * Math.Exp(-h * n / (fs * 3.0));
                    buf[i] = (float)(0.15 * env * v + 0.0005 * (rnd.NextDouble() - 0.5));
                }
                t.Write(buf);
                if (blk % 4 == 3 && t.Analyze(fs)) { var (note, oct, c) = t.Describe(t.Frequency); res = $"{t.Frequency,8:0.00} Hz  {note}{oct} {c:+0.0;-0.0} c"; }
            }
            Console.WriteLine($"in {f0,8:0.00} Hz -> {res}");
        }
        return 0;
    }

    /// <summary>Dev aid: runs the realtime paths for a few seconds at near-silent output.</summary>
    static int SelfTest(string diFile)
    {
        var m = new RigModel();
        m.Amp.Output.Value = 0.03f;
        using var host = new Audio.AudioHost(m);
        using var en = new NAudio.CoreAudioApi.MMDeviceEnumerator();
        var outDev = en.GetDefaultAudioEndpoint(NAudio.CoreAudioApi.DataFlow.Render, NAudio.CoreAudioApi.Role.Multimedia);
        if (diFile != null)
        {
            host.StartFile(diFile, outDev);
            Console.WriteLine(host.Status);
            for (int i = 0; i < 6; i++) { System.Threading.Thread.Sleep(500); Console.WriteLine($"file  in {host.Engine.InputPeak:F3} out {host.Engine.OutputPeakL:F4}/{host.Engine.OutputPeakR:F4}"); }
            host.Stop();
        }
        var inDev = en.GetDefaultAudioEndpoint(NAudio.CoreAudioApi.DataFlow.Capture, NAudio.CoreAudioApi.Role.Multimedia);
        host.StartWasapi(inDev, outDev, 0);
        Console.WriteLine(host.Status);
        for (int i = 0; i < 4; i++) { System.Threading.Thread.Sleep(500); Console.WriteLine($"wasapi in {host.Engine.InputPeak:F4} out {host.Engine.OutputPeakL:F5}"); }
        host.Stop();
        Console.WriteLine("selftest ok");
        return 0;
    }

    /// <summary>Offline test: Jwala --render in.wav out.wav ["Preset Name"] [key=value ...]</summary>
    static int Render(string[] args)
    {
        using var reader = new AudioFileReader(args[1]);
        var m = new RigModel();
        if (args.Length > 3)
        {
            var preset = FactoryPresets.All.FirstOrDefault(p => p.Name.Equals(args[3], StringComparison.OrdinalIgnoreCase));
            if (preset.Name == null) { Console.WriteLine("unknown preset " + args[3]); return 1; }
            m.Load(preset.State);
            var st = m.Save();
            foreach (var kv in args.Skip(4)) { var i = kv.IndexOf('='); st[kv[..i]] = float.Parse(kv[(i + 1)..], System.Globalization.CultureInfo.InvariantCulture); }
            m.Load(st, true);
        }
        int fs = reader.WaveFormat.SampleRate;
        var rig = new Rig(fs, m);
        int ch = reader.WaveFormat.Channels;
        using var w = new WaveFileWriter(args[2], WaveFormat.CreateIeeeFloatWaveFormat(fs, 2));
        var inter = new float[256 * ch];
        float[] mono = new float[256], l = new float[256], r = new float[256], st2 = new float[512];
        var sw = Stopwatch.StartNew();
        long total = 0;
        // render input followed by 4 s of silence so delay/reverb tails are audible
        long tail = fs * 4;
        while (true)
        {
            int read = reader.Read(inter, 0, inter.Length);
            int frames = read / ch;
            if (frames == 0) { if (tail <= 0) break; frames = (int)Math.Min(256, tail); tail -= frames; Array.Clear(mono); }
            else for (int i = 0; i < frames; i++) mono[i] = inter[i * ch];
            rig.Process(mono.AsSpan(0, frames), l.AsSpan(0, frames), r.AsSpan(0, frames));
            for (int i = 0; i < frames; i++) { st2[2 * i] = l[i]; st2[2 * i + 1] = r[i]; }
            w.WriteSamples(st2, 0, frames * 2);
            total += frames;
        }
        double audioSec = (double)total / fs;
        Console.WriteLine($"rendered {audioSec:F2}s audio in {sw.Elapsed.TotalSeconds:F2}s ({audioSec / sw.Elapsed.TotalSeconds:F0}x realtime)");
        return 0;
    }
}
