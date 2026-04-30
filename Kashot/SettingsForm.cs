namespace Kashot;

public class SettingsForm : Form
{
    private readonly AppSettings _settings;
    private readonly HotkeyTextBox _hotkeyBox;
    private readonly TextBox _saveDirBox;
    private readonly CheckBox _startupBox;
    private readonly CheckBox _watermarkBox;
    private readonly TextBox _watermarkText;
    private readonly ComboBox _themeBox;

    public event EventHandler? SettingsSaved;

    public SettingsForm(AppSettings settings)
    {
        _settings = settings;
        var c = ThemeColors.For(_settings.Theme);

        Text = "Kashot Settings";
        Size = new Size(560, 420);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = c.Background;
        ForeColor = c.Text;
        Font = new Font("Segoe UI", 9.5f);

        const int LabelX = 20;
        const int FieldX = 170;
        const int FieldRight = 540;

        // Hotkey
        var hotkeyLabel = MakeLabel("Capture hotkey", LabelX, 24, c.Text);
        _hotkeyBox = new HotkeyTextBox
        {
            Location = new Point(FieldX, 20),
            Width = FieldRight - FieldX,
            BackColor = c.Surface,
            ForeColor = c.Text,
        };
        _hotkeyBox.SetHotkey(_settings.HotkeyModifiers, _settings.HotkeyVirtualKey);
        var hotkeyHint = MakeLabel(
            "Click and press the desired key combination. Backspace clears.",
            FieldX, 48, c.TextMuted, autoSize: true);

        // Save folder
        var saveDirLabel = MakeLabel("Default save folder", LabelX, 88, c.Text);
        _saveDirBox = new TextBox
        {
            Location = new Point(FieldX, 84),
            Width = FieldRight - FieldX - 90,
            Text = _settings.SaveDirectory,
            BackColor = c.Surface,
            ForeColor = c.Text,
        };
        var browseBtn = MakeButton("Browse…", FieldRight - 80, 83, 80, c);
        browseBtn.Click += (_, _) =>
        {
            using var dlg = new FolderBrowserDialog
            {
                SelectedPath = string.IsNullOrEmpty(_saveDirBox.Text)
                    ? Environment.GetFolderPath(Environment.SpecialFolder.MyPictures)
                    : _saveDirBox.Text,
            };
            if (dlg.ShowDialog() == DialogResult.OK) _saveDirBox.Text = dlg.SelectedPath;
        };

        // Start with Windows
        _startupBox = MakeCheck("Start with Windows", FieldX, 124, _settings.StartWithWindows, c);

        // Theme
        var themeLabel = MakeLabel("Theme", LabelX, 162, c.Text);
        _themeBox = new ComboBox
        {
            Location = new Point(FieldX, 158),
            Width = 160,
            DropDownStyle = ComboBoxStyle.DropDownList,
            BackColor = c.Surface,
            ForeColor = c.Text,
            FlatStyle = FlatStyle.Flat,
        };
        _themeBox.Items.AddRange(new object[] { "Light", "Dark" });
        _themeBox.SelectedItem = string.Equals(_settings.Theme, "Dark", StringComparison.OrdinalIgnoreCase) ? "Dark" : "Light";

        // Watermark
        _watermarkBox = MakeCheck("Add watermark to images", FieldX, 198, _settings.WatermarkEnabled, c);

        var watermarkLabel = MakeLabel("Watermark text", LabelX, 232, c.Text);
        _watermarkText = new TextBox
        {
            Location = new Point(FieldX, 228),
            Width = FieldRight - FieldX,
            Text = _settings.WatermarkText,
            BackColor = c.Surface,
            ForeColor = c.Text,
        };

        // Buttons
        var ok = MakeButton("Save", FieldRight - 200, 340, 90, c);
        ok.DialogResult = DialogResult.OK;
        ok.Click += (_, _) => Apply();

        var cancel = MakeButton("Cancel", FieldRight - 100, 340, 100, c);
        cancel.DialogResult = DialogResult.Cancel;

        AcceptButton = ok;
        CancelButton = cancel;

        Controls.AddRange(new Control[]
        {
            hotkeyLabel, _hotkeyBox, hotkeyHint,
            saveDirLabel, _saveDirBox, browseBtn,
            _startupBox,
            themeLabel, _themeBox,
            _watermarkBox,
            watermarkLabel, _watermarkText,
            ok, cancel,
        });
    }

    private static Label MakeLabel(string text, int x, int y, Color color, bool autoSize = true) =>
        new()
        {
            Text = text,
            Location = new Point(x, y),
            AutoSize = autoSize,
            ForeColor = color,
            BackColor = Color.Transparent,
        };

    private static CheckBox MakeCheck(string text, int x, int y, bool isChecked, ThemeColors c) =>
        new()
        {
            Text = text,
            Location = new Point(x, y),
            AutoSize = true,
            Checked = isChecked,
            ForeColor = c.Text,
            BackColor = Color.Transparent,
        };

    private static Button MakeButton(string text, int x, int y, int width, ThemeColors c)
    {
        var b = new Button
        {
            Text = text,
            Location = new Point(x, y),
            Size = new Size(width, 30),
            FlatStyle = FlatStyle.Flat,
            BackColor = c.ButtonBg,
            ForeColor = c.Text,
            Cursor = Cursors.Hand,
        };
        b.FlatAppearance.BorderColor = c.Border;
        b.FlatAppearance.BorderSize = 1;
        b.FlatAppearance.MouseOverBackColor = c.ButtonHover;
        return b;
    }

    private void Apply()
    {
        _settings.HotkeyModifiers = _hotkeyBox.Modifiers;
        _settings.HotkeyVirtualKey = _hotkeyBox.VirtualKey;
        _settings.SaveDirectory = _saveDirBox.Text.Trim();
        _settings.StartWithWindows = _startupBox.Checked;
        _settings.WatermarkEnabled = _watermarkBox.Checked;
        _settings.WatermarkText = _watermarkText.Text;
        _settings.Theme = _themeBox.SelectedItem?.ToString() ?? "Light";
        _settings.Save();
        StartupHelper.SetEnabled(_settings.StartWithWindows);
        SettingsSaved?.Invoke(this, EventArgs.Empty);
    }
}

internal class HotkeyTextBox : TextBox
{
    public uint Modifiers { get; private set; }
    public uint VirtualKey { get; private set; }

    public HotkeyTextBox()
    {
        ReadOnly = true;
        Cursor = Cursors.IBeam;
        Text = "(none)";
    }

    public void SetHotkey(uint mods, uint vk)
    {
        Modifiers = mods;
        VirtualKey = vk;
        Text = HotkeyDisplay(mods, vk);
    }

    protected override bool IsInputKey(Keys keyData) => true;

    protected override void OnKeyDown(KeyEventArgs e)
    {
        e.SuppressKeyPress = true;
        e.Handled = true;

        var key = e.KeyCode;

        if (key == Keys.Back || key == Keys.Delete)
        {
            SetHotkey(0, 0);
            return;
        }

        if (key == Keys.ControlKey || key == Keys.ShiftKey || key == Keys.Menu ||
            key == Keys.LWin || key == Keys.RWin ||
            key == Keys.LControlKey || key == Keys.RControlKey ||
            key == Keys.LShiftKey || key == Keys.RShiftKey ||
            key == Keys.LMenu || key == Keys.RMenu ||
            key == Keys.Tab || key == Keys.Escape)
        {
            return;
        }

        uint mods = 0;
        if (e.Control) mods |= NativeMethods.MOD_CONTROL;
        if (e.Shift) mods |= NativeMethods.MOD_SHIFT;
        if (e.Alt) mods |= NativeMethods.MOD_ALT;

        SetHotkey(mods, (uint)key);
    }

    public static string HotkeyDisplay(uint mods, uint vk)
    {
        if (vk == 0) return "(none)";
        var parts = new List<string>();
        if ((mods & NativeMethods.MOD_CONTROL) != 0) parts.Add("Ctrl");
        if ((mods & NativeMethods.MOD_SHIFT) != 0) parts.Add("Shift");
        if ((mods & NativeMethods.MOD_ALT) != 0) parts.Add("Alt");
        if ((mods & NativeMethods.MOD_WIN) != 0) parts.Add("Win");
        parts.Add(KeyName((Keys)vk));
        return string.Join(" + ", parts);
    }

    private static string KeyName(Keys k) => k switch
    {
        Keys.PrintScreen => "Print Screen",
        Keys.OemPipe => @"\",
        Keys.Oemcomma => ",",
        Keys.OemPeriod => ".",
        Keys.OemQuestion => "/",
        Keys.OemSemicolon => ";",
        Keys.OemQuotes => "'",
        Keys.OemMinus => "-",
        Keys.Oemplus => "=",
        Keys.OemOpenBrackets => "[",
        Keys.OemCloseBrackets => "]",
        Keys.Oemtilde => "`",
        _ => k.ToString(),
    };
}
