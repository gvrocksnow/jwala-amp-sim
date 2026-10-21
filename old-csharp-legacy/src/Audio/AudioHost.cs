using System;
using System.Runtime.InteropServices;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using NAudio.Wave.Asio;
using Jwala.Dsp;

namespace Jwala.Audio;

public enum DriverType { Asio, Wasapi, DiFile }

public delegate void SpanFill(Span<float> dst);

/// <summary>Owns the audio devices and runs the amp engine in the audio callback.</summary>
public sealed class AudioHost : IDisposable
{
    public Rig Engine { get; private set; }
    readonly RigModel model;
    public AudioHost(RigModel m) => model = m;
    public string Status { get; private set; } = "Stopped";
    public bool Running { get; private set; }
    /// <summary>Fraction of the real-time budget spent in the DSP (smoothed).</summary>
    public double Load { get; private set; }
    readonly System.Diagnostics.Stopwatch loadClock = new();
    public void Fail(string why) => Status = "Audio off — " + why;


    AsioOut asio;
    WasapiCapture capture;
    WasapiOut output;
    AudioFileReader fileReader;
    float[] asioIn = Array.Empty<float>(), mono = Array.Empty<float>(), outL = Array.Empty<float>(), outR = Array.Empty<float>();
    RingBuffer ring;
    int captureChannel;
    
    public static string[] AsioDrivers() { try { return AsioOut.GetDriverNames(); } catch { return Array.Empty<string>(); } }

    public static MMDevice[] Devices(DataFlow flow)
    {
        using var en = new MMDeviceEnumerator();
        var list = new List<MMDevice>();
        foreach (var d in en.EnumerateAudioEndPoints(flow, DeviceState.Active)) list.Add(d);
        return list.ToArray();
    }

    void NewEngine(int rate) => Engine = new Rig(rate, model);

    /// <summary>Call after changing the cab model or loading an IR.</summary>
    public void RefreshCab() { var e = Engine; if (e != null && e.CabStale) e.UpdateCabIr(); }

    // ---------------- ASIO ----------------

    public void StartAsio(string driver, int inputChannel)
    {
        Stop();
        asio = new AsioOut(driver);
        int rate = asio.IsSampleRateSupported(48000) ? 48000 : 44100;
        asio.InputChannelOffset = Math.Clamp(inputChannel, 0, Math.Max(0, asio.DriverInputChannelCount - 1));
        asio.ChannelOffset = 0;
        NewEngine(rate);
        asio.AudioAvailable += OnAsio;
        asio.InitRecordAndPlayback(new Silence(WaveFormat.CreateIeeeFloatWaveFormat(rate, 2)), 1, rate);
        asio.Play();
        Running = true;
        Status = $"ASIO · {driver} · {rate / 1000.0:0.#} kHz · {asio.FramesPerBuffer} smp";
    }

    public void ShowAsioPanel(string driver)
    {
        if (asio != null) { asio.ShowControlPanel(); return; }
        using var a = new AsioOut(driver);
        a.ShowControlPanel();
    }

    public static string[] AsioInputNames(string driver)
    {
        try
        {
            using var a = new AsioOut(driver);
            var names = new string[a.DriverInputChannelCount];
            for (int i = 0; i < names.Length; i++) names[i] = a.AsioInputChannelName(i);
            return names;
        }
        catch { return Array.Empty<string>(); }
    }

    void OnAsio(object sender, AsioAudioAvailableEventArgs e)
    {
        int n = e.SamplesPerBuffer;
        if (asioIn.Length < n) { asioIn = new float[n]; outL = new float[n]; outR = new float[n]; }
        e.GetAsInterleavedSamples(asioIn);
        loadClock.Restart();
        Engine.Process(asioIn.AsSpan(0, n), outL.AsSpan(0, n), outR.AsSpan(0, n));
        Load += 0.05 * (loadClock.Elapsed.TotalSeconds / (n / Engine.SampleRate) - Load);
        for (int c = 0; c < e.OutputBuffers.Length; c++) WriteAsio(e.OutputBuffers[c], (c & 1) == 0 ? outL : outR, n, e.AsioSampleType);
        e.WrittenToOutputBuffers = true;
    }

    static unsafe void WriteAsio(IntPtr dst, float[] src, int n, AsioSampleType type)
    {
        switch (type)
        {
            case AsioSampleType.Float32LSB:
                Marshal.Copy(src, 0, dst, n); break;
            case AsioSampleType.Int32LSB:
                { int* d = (int*)dst; for (int i = 0; i < n; i++) d[i] = (int)(src[i] * 2147483000f); break; }
            case AsioSampleType.Int16LSB:
                { short* d = (short*)dst; for (int i = 0; i < n; i++) d[i] = (short)(src[i] * 32767f); break; }
            case AsioSampleType.Int24LSB:
                {
                    byte* d = (byte*)dst;
                    for (int i = 0; i < n; i++)
                    {
                        int v = (int)(src[i] * 8388607f);
                        d[i * 3] = (byte)v; d[i * 3 + 1] = (byte)(v >> 8); d[i * 3 + 2] = (byte)(v >> 16);
                    }
                    break;
                }
            case AsioSampleType.Int32LSB24:
                { int* d = (int*)dst; for (int i = 0; i < n; i++) d[i] = (int)(src[i] * 8388607f); break; }
            default:
                throw new NotSupportedException($"ASIO sample type {type} not supported");
        }
    }

    // ---------------- WASAPI ----------------

    public void StartWasapi(MMDevice inDev, MMDevice outDev, int inputChannel)
    {
        Stop();
        capture = new WasapiCapture(inDev, true, 5);
        var cf = capture.WaveFormat;
        captureChannel = Math.Clamp(inputChannel, 0, cf.Channels - 1);
        NewEngine(cf.SampleRate);
        ring = new RingBuffer(cf.SampleRate); // 1 s capacity
        capture.DataAvailable += OnCapture;
        output = new WasapiOut(outDev, AudioClientShareMode.Shared, true, 10);
        output.Init(new EngineProvider(this, cf.SampleRate, FillFromRing));
        capture.StartRecording();
        output.Play();
        Running = true;
        Status = $"WASAPI · {inDev.FriendlyName} · in {captureChannel + 1}";
    }

    void OnCapture(object sender, WaveInEventArgs e)
    {
        var f = capture.WaveFormat;
        int bps = f.BitsPerSample / 8, frame = bps * f.Channels, frames = e.BytesRecorded / frame;
        Span<float> tmp = frames <= 4096 ? stackalloc float[frames] : new float[frames];
        bool isFloat = f.Encoding == WaveFormatEncoding.IeeeFloat ||
                       (f is WaveFormatExtensible x && x.SubFormat == NAudio.Dmo.AudioMediaSubtypes.MEDIASUBTYPE_IEEE_FLOAT);
        for (int i = 0; i < frames; i++)
        {
            int o = i * frame + captureChannel * bps;
            tmp[i] = bps switch
            {
                4 when isFloat => BitConverter.ToSingle(e.Buffer, o),
                4 => BitConverter.ToInt32(e.Buffer, o) / 2147483648f,
                3 => ((e.Buffer[o] << 8 | e.Buffer[o + 1] << 16 | e.Buffer[o + 2] << 24) >> 8) / 8388608f,
                2 => BitConverter.ToInt16(e.Buffer, o) / 32768f,
                _ => 0,
            };
        }
        ring.Write(tmp);
    }

    void FillFromRing(Span<float> dst)
    {
        // keep latency bounded: if the capture side got ahead, skip the backlog
        int target = dst.Length + (int)(Engine.SampleRate * 0.004);
        if (ring.Count > target * 3) ring.Skip(ring.Count - target);
        int got = ring.Read(dst);
        dst.Slice(got).Clear();
    }

    // ---------------- DI file ----------------

    public void StartFile(string path, MMDevice outDev)
    {
        Stop();
        fileReader = new AudioFileReader(path);
        NewEngine(fileReader.WaveFormat.SampleRate);
        output = new WasapiOut(outDev, AudioClientShareMode.Shared, true, 30);
        output.Init(new EngineProvider(this, fileReader.WaveFormat.SampleRate, FillFromFile));
        output.Play();
        Running = true;
        Status = $"DI · {System.IO.Path.GetFileName(path)}";
    }

    float[] fileTmp = Array.Empty<float>();
    void FillFromFile(Span<float> dst)
    {
        int ch = fileReader.WaveFormat.Channels;
        if (fileTmp.Length < dst.Length * ch) fileTmp = new float[dst.Length * ch];
        int filled = 0;
        while (filled < dst.Length)
        {
            int read = fileReader.Read(fileTmp, 0, (dst.Length - filled) * ch);
            if (read == 0) { fileReader.Position = 0; continue; }
            for (int i = 0; i < read / ch; i++) dst[filled + i] = fileTmp[i * ch];
            filled += read / ch;
        }
    }

    // ---------------- common ----------------

    public void Stop()
    {
        Running = false;
        try { asio?.Stop(); } catch { }
        try { capture?.StopRecording(); } catch { }
        try { output?.Stop(); } catch { }
        asio?.Dispose(); capture?.Dispose(); output?.Dispose(); fileReader?.Dispose();
        asio = null; capture = null; output = null; fileReader = null;
        Status = "Stopped";
        Load = 0;
    }

    public void Dispose() => Stop();

    /// <summary>Stereo float provider that pulls mono input, runs the amp, duplicates to L/R.</summary>
    sealed class EngineProvider : ISampleProvider
    {
        readonly AudioHost host; readonly SpanFill source; float[] buf = Array.Empty<float>(), bl = Array.Empty<float>(), br = Array.Empty<float>();
        public WaveFormat WaveFormat { get; }
        public EngineProvider(AudioHost h, int rate, SpanFill src)
        { host = h; source = src; WaveFormat = WaveFormat.CreateIeeeFloatWaveFormat(rate, 2); }

        public int Read(float[] buffer, int offset, int count)
        {
            int frames = count / 2;
            if (buf.Length < frames) { buf = new float[frames]; bl = new float[frames]; br = new float[frames]; }
            var span = buf.AsSpan(0, frames);
            source(span);
            host.loadClock.Restart();
            host.Engine.Process(span, bl.AsSpan(0, frames), br.AsSpan(0, frames));
            host.Load += 0.05 * (host.loadClock.Elapsed.TotalSeconds / (frames / host.Engine.SampleRate) - host.Load);
            for (int i = 0; i < frames; i++) { buffer[offset + 2 * i] = bl[i]; buffer[offset + 2 * i + 1] = br[i]; }
            return frames * 2;
        }
    }

    sealed class Silence : IWaveProvider
    {
        public WaveFormat WaveFormat { get; }
        public Silence(WaveFormat f) => WaveFormat = f;
        public int Read(byte[] buffer, int offset, int count) { Array.Clear(buffer, offset, count); return count; }
    }

    sealed class RingBuffer
    {
        readonly float[] data; int r, w, count; readonly object gate = new();
        public RingBuffer(int capacity) => data = new float[capacity];
        public int Count { get { lock (gate) return count; } }
        public void Write(ReadOnlySpan<float> src)
        {
            lock (gate)
                foreach (var v in src)
                {
                    data[w] = v; w = (w + 1) % data.Length;
                    if (count < data.Length) count++; else r = (r + 1) % data.Length;
                }
        }
        public int Read(Span<float> dst)
        {
            lock (gate)
            {
                int n = Math.Min(dst.Length, count);
                for (int i = 0; i < n; i++) { dst[i] = data[r]; r = (r + 1) % data.Length; }
                count -= n; return n;
            }
        }
        public void Skip(int n) { lock (gate) { n = Math.Min(n, count); r = (r + n) % data.Length; count -= n; } }
    }
}
