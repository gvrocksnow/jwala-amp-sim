using System;
using System.Collections.Generic;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Windows.Forms;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using Jwala.Audio;
using Jwala.Dsp;
using Jwala.UI;

namespace Jwala;

public sealed class Settings
{
    public DriverType Driver { get; set; } = AudioHost.AsioDrivers().Length > 0 ? DriverType.Asio : DriverType.Wasapi;
    public string AsioDriver { get; set; }
    public string InputDevice { get; set; }
    public string OutputDevice { get; set; }
    public int InputChannel { get; set; }
    public string DiFile { get; set; }
    public string IrFile { get; set; }
    public string Preset { get; set; } = "Classic Crunch";
    public int Page { get; set; } = 1;
    public Dictionary<string, float> State { get; set; }

    public static string Dir => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "Jwala");
    static string PathOf => Path.Combine(Dir, "settings.json");
    public static Settings Load() { try { return JsonSerializer.Deserialize<Settings>(File.ReadAllText(PathOf)) ?? new(); } catch { return new(); } }
    public void Save() { try { Directory.CreateDirectory(Dir); File.WriteAllText(PathOf, JsonSerializer.Serialize(this)); } catch { } }
}

/// <summary>User presets live as JSON files next to the settings.</summary>
static class PresetStore
{
    static string Folder => Path.Combine(Settings.Dir, "presets");
    public static string[] UserNames()
    {
        try { return Directory.GetFiles(Folder, "*.json").Select(Path.GetFileNameWithoutExtension).OrderBy(n => n).ToArray(); }
        catch { return Array.Empty<string>(); }
    }
    public static Dictionary<string, float> LoadUser(string name)
    {
        try { return JsonSerializer.Deserialize<Dictionary<string, float>>(File.ReadAllText(Path.Combine(Folder, name + ".json"))); } catch { return null; }
    }
    public static void Save(string name, Dictionary<string, float> state)
    {
        Directory.CreateDirectory(Folder);
        File.WriteAllText(Path.Combine(Folder, Safe(name) + ".json"), JsonSerializer.Serialize(state, new JsonSerializerOptions { WriteIndented = true }));
    }
    public static void Delete(string name) { try { File.Delete(Path.Combine(Folder, name + ".json")); } catch { } }
    public static string Safe(string n) => string.Concat(n.Split(Path.GetInvalidFileNameChars())).Trim();
    public static bool IsFactory(string n) => FactoryPresets.All.Any(p => p.Name == n);

    /// <summary>All preset names in browse order: factory first, then user.</summary>
    public static List<string> All() => FactoryPresets.All.Select(p => p.Name).Concat(UserNames()).ToList();

    public static Dictionary<string, float> Get(string name) =>
        FactoryPresets.All.FirstOrDefault(p => p.Name == name).State ?? LoadUser(name);
}

public sealed class MainForm : Form
{
    readonly RigModel model = new();
    readonly AudioHost host;
    readonly Settings s = Settings.Load();
    readonly HeaderBar header;
    readonly WidgetSurface[] pages;
    readonly Panel content = new() { Dock = DockStyle.Fill, BackColor = Color.Black };
    readonly bool autoStart;
    readonly TunerPanel tuner;
    string presetName; bool dirty;
    System.Threading.Timer saveDebounce;

    public MainForm(bool autoStart)
    {
        this.autoStart = autoStart;
        host = new AudioHost(model);
        Text = "Jwala — Amp & Effects Rig";
        // sizes in logical pixels, scaled to the monitor like the UI itself (WidgetSurface)
        float dpi = DeviceDpi / 96f;
        ClientSize = new Size((int)(1320 * dpi), (int)(820 * dpi));
        MinimumSize = new Size((int)(1180 * dpi), (int)(720 * dpi));
        BackColor = Color.Black;
        StartPosition = FormStartPosition.CenterScreen;
        try { Icon = Icon.ExtractAssociatedIcon(Application.ExecutablePath); } catch { }

        // restore last state (or the default preset on first run)
        presetName = s.Preset ?? "Classic Crunch";
        if (s.State != null) model.Load(s.State, includeGlobals: true);
        else model.Load(PresetStore.Get(presetName) ?? new());
        if (!string.IsNullOrEmpty(s.IrFile) && File.Exists(s.IrFile)) TryLoadIr(s.IrFile, select: false);
        if (s.State != null && s.State.TryGetValue("cab.MODEL", out var cm) && (int)cm == 3 && model.Cab.IrData != null) model.Cab.Model = 3;

        header = new HeaderBar(model)
        {
            Dock = DockStyle.Top,
            PresetName = () => presetName + (dirty ? " *" : ""),
            PowerOn = () => host.Running,
            Status = () => host.Status,
            Engine = () => host.Engine,
            CpuLoad = () => host.Load,
            TunerOn = () => model.Tuner.Active,
        };
        tuner = new TunerPanel(model.Tuner) { Dock = DockStyle.Fill, Visible = false, Engine = () => host.Engine, PowerOn = () => host.Running };
        tuner.CloseRequested += () => SetTuner(false);
        header.ToggleTuner += () => SetTuner(!model.Tuner.Active);
        var amp = new AmpPanel(model) { PowerOn = () => host.Running };
        var cabPage = new CabPage(model);
        pages = new WidgetSurface[]
        {
            new PedalPage(model.Pre, "PRE-AMP PEDALS", "Dynamics and drive in front of the amp — click a footswitch to engage, drag knobs (Shift = fine, double-click = reset)"),
            amp,
            cabPage,
            new PedalPage(model.Post, "POST EFFECTS", "Stereo doubler, modulation, delay and reverb after the cab — delay and reverb tails ring out when switched off"),
        };
        foreach (var p in pages) { p.Dock = DockStyle.Fill; p.Visible = false; p.ParamsChanged += _ => OnParamsChanged(); content.Controls.Add(p); }
        header.ParamsChanged += _ => OnParamsChanged();

        amp.PowerClicked += TogglePower;
        cabPage.CabChanged += () => { host.RefreshCab(); OnParamsChanged(); };
        cabPage.LoadIrRequested += LoadIrDialog;
        header.TogglePower += TogglePower;
        header.OpenSettings += OpenSettings;
        header.PageSelected += ShowPage;
        header.PrevPreset += () => StepPreset(-1);
        header.NextPreset += () => StepPreset(1);
        header.SavePreset += SavePreset;
        header.PresetMenu += ShowPresetMenu;

        content.Controls.Add(tuner);
        Controls.Add(content);
        Controls.Add(header);
        ShowPage(Math.Clamp(s.Page, 0, 3));

        var timer = new System.Windows.Forms.Timer { Interval = 33 };
        timer.Tick += (_, _) => { header.Tick(); if (tuner.Visible) tuner.Tick(); else pages[header.Page].Tick(); };
        timer.Start();

        KeyPreview = true;
        KeyDown += (_, e) =>
        {
            if (e.KeyCode == Keys.Space) { TogglePower(); e.Handled = true; }
            else if (e.KeyCode == Keys.T) { SetTuner(!model.Tuner.Active); e.Handled = true; }
            else if (e.KeyCode == Keys.Escape && model.Tuner.Active) { SetTuner(false); e.Handled = true; }
            else if (e.KeyCode >= Keys.D1 && e.KeyCode <= Keys.D4) { ShowPage(e.KeyCode - Keys.D1); e.Handled = true; }
            else if (e.KeyCode == Keys.Left && e.Control) StepPreset(-1);
            else if (e.KeyCode == Keys.Right && e.Control) StepPreset(1);
        };
        // power is on by default: start audio as soon as the window is up
        Shown += (_, _) => { if (this.autoStart) StartAudio(interactive: false); };
    }

    public void OpenTunerForScreenshot() => SetTuner(true);

    public void ShowPage(string name) => ShowPage(name.ToLowerInvariant() switch { "pedals" => 0, "amp" => 1, "cab" => 2, "effects" => 3, _ => 1 });

    /// <summary>Opens/closes the tuner overlay; output is muted while it's open (if enabled there).</summary>
    void SetTuner(bool on)
    {
        model.Tuner.Active = on;
        tuner.Visible = on;
        if (on) tuner.BringToFront();
        pages[header.Page].Visible = !on;
        header.Invalidate();
    }

    void ShowPage(int i)
    {
        if (model.Tuner.Active) { model.Tuner.Active = false; tuner.Visible = false; }
        header.Page = i;
        for (int k = 0; k < pages.Length; k++) pages[k].Visible = k == i;
        header.Invalidate();
        s.Page = i; SaveSoon();
    }

    // ------------------------------------------------------------------ audio

    void TogglePower()
    {
        if (host.Running) host.Stop();
        else StartAudio(interactive: true);
        RefreshPower();
    }

    void StartAudio(bool interactive)
    {
        try
        {
            switch (s.Driver)
            {
                case DriverType.Asio:
                    var drivers = AudioHost.AsioDrivers();
                    if (drivers.Length == 0) throw new InvalidOperationException("no ASIO driver — open ⚙ Settings");
                    host.StartAsio(drivers.Contains(s.AsioDriver) ? s.AsioDriver : GuessAsioDriver(drivers), s.InputChannel);
                    break;
                case DriverType.Wasapi:
                    host.StartWasapi(FindDevice(DataFlow.Capture, s.InputDevice), FindDevice(DataFlow.Render, s.OutputDevice), s.InputChannel);
                    break;
                case DriverType.DiFile:
                    if (string.IsNullOrEmpty(s.DiFile) || !File.Exists(s.DiFile)) throw new InvalidOperationException("no DI file chosen — open ⚙ Settings");
                    host.StartFile(s.DiFile, FindDevice(DataFlow.Render, s.OutputDevice));
                    break;
            }
        }
        catch (Exception ex)
        {
            host.Stop();
            host.Fail(ex.Message);
            if (interactive) MessageBox.Show(this, ex.Message, "Jwala", MessageBoxButtons.OK, MessageBoxIcon.Warning);
        }
        RefreshPower();
    }

    /// <summary>First run: pick the ASIO driver that matches the default recording device (e.g. "Focusrite").</summary>
    static string GuessAsioDriver(string[] drivers)
    {
        try
        {
            using var en = new MMDeviceEnumerator();
            string input = en.GetDefaultAudioEndpoint(DataFlow.Capture, Role.Multimedia).FriendlyName;
            var match = drivers.FirstOrDefault(d => input.Contains(d.Split(' ')[0], StringComparison.OrdinalIgnoreCase));
            if (match != null) return match;
        }
        catch { }
        return drivers.FirstOrDefault(d => d.Contains("ASIO4ALL")) ?? drivers[0];
    }

    static MMDevice FindDevice(DataFlow flow, string name)
    {
        var d = AudioHost.Devices(flow).FirstOrDefault(x => x.FriendlyName == name);
        if (d != null) return d;
        using var en = new MMDeviceEnumerator();
        return en.GetDefaultAudioEndpoint(flow, Role.Multimedia);
    }

    void RefreshPower()
    {
        header.Invalidate();
        ((AmpPanel)pages[1]).RefreshPower();
    }

    void OpenSettings()
    {
        bool wasRunning = host.Running;
        using var dlg = new SettingsForm(s, host);
        if (dlg.ShowDialog(this) == DialogResult.OK)
        {
            host.Stop();
            s.Save();
            StartAudio(interactive: true);
        }
        else if (wasRunning && !host.Running) StartAudio(interactive: false);
    }

    // ------------------------------------------------------------------ presets

    void ApplyPreset(string name)
    {
        var st = PresetStore.Get(name);
        if (st == null) return;
        model.Load(st);
        presetName = name; dirty = false;
        host.RefreshCab();
        foreach (var p in pages) p.Relayout();
        header.Invalidate();
        SaveSoon();
    }

    void StepPreset(int dir)
    {
        var all = PresetStore.All();
        int i = all.IndexOf(presetName);
        ApplyPreset(all[((i < 0 ? 0 : i + dir) % all.Count + all.Count) % all.Count]);
    }

    void ShowPresetMenu(RectangleF at)
    {
        var menu = new ContextMenuStrip { Renderer = new ToolStripProfessionalRenderer(new Theme.MenuColors()), ShowImageMargin = false, ShowCheckMargin = true, Font = new Font("Segoe UI", 9.5f) };
        ToolStripItem Head(string t) => new ToolStripLabel(t) { ForeColor = Theme.Gold, Font = new Font("Segoe UI", 8f, FontStyle.Bold) };
        ToolStripMenuItem Item(string n) => new(n, null, (_, _) => ApplyPreset(n)) { ForeColor = Theme.Text, Checked = n == presetName };
        menu.Items.Add(Head("FACTORY"));
        foreach (var p in FactoryPresets.All) menu.Items.Add(Item(p.Name));
        var user = PresetStore.UserNames();
        if (user.Length > 0)
        {
            menu.Items.Add(new ToolStripSeparator());
            menu.Items.Add(Head("USER"));
            foreach (var n in user) menu.Items.Add(Item(n));
        }
        if (user.Contains(presetName))
        {
            menu.Items.Add(new ToolStripSeparator());
            menu.Items.Add(new ToolStripMenuItem($"Delete \"{presetName}\"", null, (_, _) =>
            {
                if (MessageBox.Show(this, $"Delete preset \"{presetName}\"?", "Jwala", MessageBoxButtons.YesNo) == DialogResult.Yes)
                { PresetStore.Delete(presetName); dirty = true; header.Invalidate(); }
            }) { ForeColor = Color.FromArgb(240, 140, 120) });
        }
        menu.Show(header, Point.Round(new PointF(at.X * header.UiScale, at.Y * header.UiScale)));
    }

    void SavePreset()
    {
        using var dlg = new NameDialog("Save Preset", PresetStore.IsFactory(presetName) ? presetName + " (mine)" : presetName);
        if (dlg.ShowDialog(this) != DialogResult.OK || dlg.Value.Length == 0) return;
        string name = PresetStore.Safe(dlg.Value);
        if (PresetStore.IsFactory(name)) { MessageBox.Show(this, "That name belongs to a factory preset — pick another.", "Jwala"); return; }
        PresetStore.Save(name, model.Save());
        presetName = name; dirty = false;
        header.Invalidate(); SaveSoon();
    }

    // ------------------------------------------------------------------ IR

    void LoadIrDialog()
    {
        using var dlg = new OpenFileDialog { Filter = "Impulse responses (*.wav)|*.wav|All files|*.*", Title = "Load a cabinet impulse response" };
        if (dlg.ShowDialog(this) != DialogResult.OK) return;
        TryLoadIr(dlg.FileName, select: true);
        foreach (var p in pages) p.Relayout();
        OnParamsChanged();
    }

    void TryLoadIr(string path, bool select)
    {
        try
        {
            using var r = new AudioFileReader(path);
            int ch = r.WaveFormat.Channels;
            var all = new float[Math.Min(r.Length / 4, Convolver.MaxLength * 8 * ch)];
            int n = r.Read(all, 0, all.Length) / ch;
            var ir = new float[n];
            for (int i = 0; i < n; i++) ir[i] = all[i * ch];
            model.Cab.IrRate = r.WaveFormat.SampleRate;
            model.Cab.IrData = ir;
            model.Cab.IrPath = path;
            s.IrFile = path;
            if (select) { model.Cab.Model = 3; host.RefreshCab(); }
        }
        catch (Exception ex) { if (select) MessageBox.Show(this, ex.Message, "Jwala", MessageBoxButtons.OK, MessageBoxIcon.Warning); }
    }

    // ------------------------------------------------------------------ persistence

    void OnParamsChanged()
    {
        dirty = true;
        header.Invalidate();
        SaveSoon();
    }

    void SaveSoon()
    {
        if (!autoStart) return; // dev screenshot mode never touches the user's settings
        saveDebounce?.Dispose();
        saveDebounce = new System.Threading.Timer(_ => SaveNow(), null, 600, System.Threading.Timeout.Infinite);
    }

    void SaveNow()
    {
        s.State = model.Save();
        s.Preset = presetName;
        s.Save();
    }

    protected override void OnFormClosing(FormClosingEventArgs e)
    {
        host.Stop();
        saveDebounce?.Dispose();
        if (autoStart) SaveNow();
        base.OnFormClosing(e);
    }
}
