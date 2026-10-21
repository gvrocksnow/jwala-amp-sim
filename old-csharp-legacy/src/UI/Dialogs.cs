using System;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Windows.Forms;
using NAudio.CoreAudioApi;
using Jwala.Audio;

namespace Jwala.UI;

static class Theme
{
    public static readonly Color Back = Color.FromArgb(24, 21, 19), Field = Color.FromArgb(40, 36, 32), Text = Color.FromArgb(235, 225, 205), Gold = Color.FromArgb(214, 178, 96);

    public static Label Label(string t, bool gold = false) => new() { Text = t, AutoSize = true, ForeColor = gold ? Gold : Text, Font = new Font("Segoe UI", 9f, gold ? FontStyle.Bold : FontStyle.Regular), Margin = new Padding(3, 8, 3, 3) };
    public static ComboBox Combo() => new() { Width = 330, DropDownStyle = ComboBoxStyle.DropDownList, FlatStyle = FlatStyle.Flat, BackColor = Field, ForeColor = Text, Font = new Font("Segoe UI", 9f) };
    public static Button Button(string t) => new() { Text = t, AutoSize = true, FlatStyle = FlatStyle.Flat, BackColor = Field, ForeColor = Text, Font = new Font("Segoe UI", 9f), Padding = new Padding(6, 2, 6, 2), FlatAppearance = { BorderColor = Color.FromArgb(90, 78, 60) } };

    /// <summary>Dark colours for context menus.</summary>
    public sealed class MenuColors : ProfessionalColorTable
    {
        public override Color ToolStripDropDownBackground => Color.FromArgb(30, 27, 24);
        public override Color ImageMarginGradientBegin => Color.FromArgb(30, 27, 24);
        public override Color ImageMarginGradientMiddle => Color.FromArgb(30, 27, 24);
        public override Color ImageMarginGradientEnd => Color.FromArgb(30, 27, 24);
        public override Color MenuItemSelected => Color.FromArgb(70, 58, 36);
        public override Color MenuItemBorder => Gold;
        public override Color MenuBorder => Color.FromArgb(80, 70, 56);
        public override Color SeparatorDark => Color.FromArgb(70, 62, 52);
        public override Color SeparatorLight => Color.FromArgb(30, 27, 24);
    }
}

/// <summary>Audio device configuration.</summary>
sealed class SettingsForm : Form
{
    readonly Settings s;
    readonly ComboBox cbDriver = Theme.Combo(), cbInput = Theme.Combo(), cbChannel = Theme.Combo(), cbOutput = Theme.Combo();
    readonly Button btnPanel = Theme.Button("ASIO Control Panel…"), btnDi = Theme.Button("Choose DI file…"), btnOk = Theme.Button("Apply && Start"), btnCancel = Theme.Button("Cancel");
    readonly Label lblOutput = Theme.Label("Output device"), lblChannel = Theme.Label("Input channel"), lblHint = Theme.Label("");
    MMDevice[] inDevs = Array.Empty<MMDevice>(), outDevs = Array.Empty<MMDevice>();
    bool loading;
    public MMDevice SelectedInput => s.Driver == DriverType.Wasapi && cbInput.SelectedIndex >= 0 && cbInput.SelectedIndex < inDevs.Length ? inDevs[cbInput.SelectedIndex] : null;
    public MMDevice SelectedOutput => cbOutput.SelectedIndex >= 0 && cbOutput.SelectedIndex < outDevs.Length ? outDevs[cbOutput.SelectedIndex] : null;

    public SettingsForm(Settings settings, AudioHost host)
    {
        s = settings;
        Text = "Audio Settings"; BackColor = Theme.Back; ForeColor = Theme.Text;
        FormBorderStyle = FormBorderStyle.FixedDialog; MaximizeBox = MinimizeBox = false; StartPosition = FormStartPosition.CenterParent;
        AutoSize = true; AutoSizeMode = AutoSizeMode.GrowAndShrink; Padding = new Padding(16);

        var t = new TableLayoutPanel { ColumnCount = 2, AutoSize = true, Dock = DockStyle.Fill };
        void Row(Control a, Control b) { t.Controls.Add(a); t.Controls.Add(b); }
        Row(Theme.Label("Driver", true), cbDriver);
        Row(Theme.Label("Input"), cbInput);
        Row(lblChannel, cbChannel);
        Row(lblOutput, cbOutput);
        var tools = new FlowLayoutPanel { AutoSize = true };
        tools.Controls.AddRange(new Control[] { btnPanel, btnDi });
        Row(new Label(), tools);
        lblHint.ForeColor = Color.FromArgb(160, 150, 130); lblHint.MaximumSize = new Size(340, 0);
        Row(new Label(), lblHint);
        var buttons = new FlowLayoutPanel { AutoSize = true, FlowDirection = FlowDirection.RightToLeft, Dock = DockStyle.Fill };
        buttons.Controls.AddRange(new Control[] { btnOk, btnCancel });
        t.Controls.Add(buttons, 0, 6); t.SetColumnSpan(buttons, 2);
        Controls.Add(t);
        AcceptButton = btnOk; CancelButton = btnCancel;
        btnOk.DialogResult = DialogResult.OK; btnCancel.DialogResult = DialogResult.Cancel;
        btnOk.BackColor = Color.FromArgb(70, 56, 30);

        cbDriver.Items.AddRange(new object[] { "ASIO (lowest latency)", "WASAPI (Windows audio)", "DI File (play a recording)" });
        cbDriver.SelectedIndexChanged += (_, _) => { if (!loading) { s.Driver = (DriverType)cbDriver.SelectedIndex; Refill(); } };
        cbInput.SelectedIndexChanged += (_, _) => { if (!loading) { Remember(); RefillChannels(); } };
        cbChannel.SelectedIndexChanged += (_, _) => { if (!loading) s.InputChannel = Math.Max(0, cbChannel.SelectedIndex); };
        cbOutput.SelectedIndexChanged += (_, _) => { if (!loading) Remember(); };
        btnPanel.Click += (_, _) => { try { host.ShowAsioPanel(cbInput.Text); } catch (Exception ex) { MessageBox.Show(this, ex.Message, "ASIO"); } };
        btnDi.Click += (_, _) =>
        {
            using var dlg = new OpenFileDialog { Filter = "Audio files|*.wav;*.mp3;*.flac;*.aiff;*.aif|All files|*.*", Title = "Load a dry DI guitar recording" };
            if (dlg.ShowDialog(this) == DialogResult.OK) { s.DiFile = dlg.FileName; s.Driver = DriverType.DiFile; loading = true; cbDriver.SelectedIndex = 2; loading = false; Refill(); }
        };
        FormClosing += (_, e) => { if (DialogResult == DialogResult.OK) Remember(); };

        loading = true; cbDriver.SelectedIndex = (int)s.Driver; loading = false;
        Refill();
    }

    void Refill()
    {
        loading = true;
        cbInput.Items.Clear(); cbOutput.Items.Clear();
        switch (s.Driver)
        {
            case DriverType.Asio:
                var drivers = AudioHost.AsioDrivers();
                cbInput.Items.AddRange(drivers);
                Select(cbInput, s.AsioDriver);
                lblHint.Text = drivers.Length == 0 ? "No ASIO drivers found. Install your interface's driver (or ASIO4ALL), or use WASAPI." : "Set the buffer size in the ASIO control panel (64–128 samples feels best).";
                break;
            case DriverType.Wasapi:
                inDevs = AudioHost.Devices(DataFlow.Capture);
                cbInput.Items.AddRange(inDevs.Select(d => (object)d.FriendlyName).ToArray());
                Select(cbInput, s.InputDevice);
                lblHint.Text = "WASAPI adds roughly 20–30 ms of latency. Use ASIO if your interface has a driver.";
                break;
            default:
                cbInput.Items.Add(string.IsNullOrEmpty(s.DiFile) ? "(choose a DI file)" : Path.GetFileName(s.DiFile));
                cbInput.SelectedIndex = 0;
                lblHint.Text = "Loops a dry guitar recording through the rig.";
                break;
        }
        bool asio = s.Driver == DriverType.Asio;
        cbOutput.Visible = lblOutput.Visible = !asio;
        btnPanel.Visible = asio;
        cbChannel.Visible = lblChannel.Visible = s.Driver != DriverType.DiFile;
        if (!asio)
        {
            outDevs = AudioHost.Devices(DataFlow.Render);
            cbOutput.Items.AddRange(outDevs.Select(d => (object)d.FriendlyName).ToArray());
            if (!Select(cbOutput, s.OutputDevice))
                try { using var en = new MMDeviceEnumerator(); Select(cbOutput, en.GetDefaultAudioEndpoint(DataFlow.Render, Role.Multimedia).FriendlyName); } catch { }
        }
        loading = false;
        Remember();
        RefillChannels();
    }

    void RefillChannels()
    {
        loading = true;
        cbChannel.Items.Clear();
        if (s.Driver == DriverType.Asio && cbInput.SelectedIndex >= 0)
            cbChannel.Items.AddRange(AudioHost.AsioInputNames(cbInput.Text).Select((n, i) => (object)$"{i + 1}: {n}").ToArray());
        else if (s.Driver == DriverType.Wasapi && SelectedInput != null)
        {
            int chs = 2;
            try { chs = SelectedInput.AudioClient.MixFormat.Channels; } catch { }
            for (int i = 0; i < chs; i++) cbChannel.Items.Add($"Input {i + 1}");
        }
        if (cbChannel.Items.Count > 0) cbChannel.SelectedIndex = Math.Clamp(s.InputChannel, 0, cbChannel.Items.Count - 1);
        loading = false;
    }

    static bool Select(ComboBox cb, string name)
    {
        int i = name == null ? -1 : cb.Items.IndexOf(name);
        if (i >= 0) { cb.SelectedIndex = i; return true; }
        if (cb.Items.Count > 0 && cb.SelectedIndex < 0) cb.SelectedIndex = 0;
        return false;
    }

    void Remember()
    {
        if (s.Driver == DriverType.Asio && cbInput.SelectedIndex >= 0) s.AsioDriver = cbInput.Text;
        else if (s.Driver == DriverType.Wasapi && cbInput.SelectedIndex >= 0) s.InputDevice = cbInput.Text;
        if (s.Driver != DriverType.Asio && cbOutput.SelectedIndex >= 0) s.OutputDevice = cbOutput.Text;
        if (cbChannel.SelectedIndex >= 0) s.InputChannel = cbChannel.SelectedIndex;
    }
}

/// <summary>Tiny "name your preset" prompt.</summary>
sealed class NameDialog : Form
{
    readonly TextBox box = new() { Width = 280, BackColor = Theme.Field, ForeColor = Theme.Text, BorderStyle = BorderStyle.FixedSingle, Font = new Font("Segoe UI", 10f) };
    public string Value => box.Text.Trim();
    public NameDialog(string title, string initial)
    {
        Text = title; BackColor = Theme.Back; FormBorderStyle = FormBorderStyle.FixedDialog; MaximizeBox = MinimizeBox = false;
        StartPosition = FormStartPosition.CenterParent; AutoSize = true; AutoSizeMode = AutoSizeMode.GrowAndShrink; Padding = new Padding(14);
        box.Text = initial;
        var ok = Theme.Button("Save"); ok.DialogResult = DialogResult.OK;
        var cancel = Theme.Button("Cancel"); cancel.DialogResult = DialogResult.Cancel;
        var f = new FlowLayoutPanel { AutoSize = true, FlowDirection = FlowDirection.TopDown };
        var b = new FlowLayoutPanel { AutoSize = true };
        b.Controls.AddRange(new Control[] { ok, cancel });
        f.Controls.AddRange(new Control[] { Theme.Label("Preset name", true), box, b });
        Controls.Add(f);
        AcceptButton = ok; CancelButton = cancel;
    }
}
