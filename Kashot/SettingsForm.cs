namespace Kashot;

public class SettingsForm : Form
{
    private readonly AppSettings _settings;
    private readonly HotkeyTextBox _hotkeyBox;
    private readonly TextBox _saveDirBox;
    private readonly CheckBox _startupBox;
    private readonly CheckBox _watermarkBox;
    private readonly TextBox _watermarkText;

    public event EventHandler? SettingsSaved;

    public SettingsForm(AppSettings settings)
    {
        _settings = settings;

        Text = "Kashot Settings";
        Size = new Size(440, 340);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(245, 245, 245);
        Font = new Font("Segoe UI", 9f);

        var hotkeyLabel = new Label { Text = "Capture hotkey", Location = new Point(20, 22), AutoSize = true };
        _hotkeyBox = new HotkeyTextBox { Location = new Point(150, 18), Width = 220 };
        _hotkeyBox.SetHotkey(_settings.HotkeyModifiers, _settings.HotkeyVirtualKey);
        var hotkeyHint = new Label
        {
            Text = "Click and press the desired key combination. Backspace clears.",
            Location = new Point(150, 44),
            AutoSize = true,
            ForeColor = Color.Gray,
        };

        var saveDirLabel = new Label { Text = "Default save folder", Location = new Point(20, 82), AutoSize = true };
        _saveDirBox = new TextBox { Location = new Point(150, 78), Width = 180, Text = _settings.SaveDirectory };
        var browseBtn = new Button { Text = "Browse…", Location = new Point(335, 77), Width = 70 };
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

        _startupBox = new CheckBox
        {
            Text = "Start with Windows",
            Location = new Point(150, 116),
            AutoSize = true,
            Checked = _settings.StartWithWindows,
        };

        _watermarkBox = new CheckBox
        {
            Text = "Add watermark to saved / copied / pinned images",
            Location = new Point(150, 148),
            AutoSize = true,
            Checked = _settings.WatermarkEnabled,
        };

        var watermarkLabel = new Label { Text = "Watermark text", Location = new Point(20, 182), AutoSize = true };
        _watermarkText = new TextBox
        {
            Location = new Point(150, 178),
            Width = 220,
            Text = _settings.WatermarkText,
        };

        var ok = new Button
        {
            Text = "Save",
            Location = new Point(220, 250),
            Width = 80,
            DialogResult = DialogResult.OK,
        };
        ok.Click += (_, _) => Apply();

        var cancel = new Button
        {
            Text = "Cancel",
            Location = new Point(310, 250),
            Width = 80,
            DialogResult = DialogResult.Cancel,
        };

        AcceptButton = ok;
        CancelButton = cancel;

        Controls.AddRange(new Control[]
        {
            hotkeyLabel, _hotkeyBox, hotkeyHint,
            saveDirLabel, _saveDirBox, browseBtn,
            _startupBox,
            _watermarkBox,
            watermarkLabel, _watermarkText,
            ok, cancel,
        });
    }

    private void Apply()
    {
        _settings.HotkeyModifiers = _hotkeyBox.Modifiers;
        _settings.HotkeyVirtualKey = _hotkeyBox.VirtualKey;
        _settings.SaveDirectory = _saveDirBox.Text.Trim();
        _settings.StartWithWindows = _startupBox.Checked;
        _settings.WatermarkEnabled = _watermarkBox.Checked;
        _settings.WatermarkText = _watermarkText.Text;
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
        BackColor = Color.White;
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
