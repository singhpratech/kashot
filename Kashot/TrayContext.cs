using System.Drawing.Drawing2D;

namespace Kashot;

public class TrayContext : ApplicationContext
{
    private readonly AppSettings _settings;
    private readonly NotifyIcon _trayIcon;
    private readonly HotkeyWindow _hotkeyWindow;
    private OverlayForm? _overlay;

    public TrayContext()
    {
        _settings = AppSettings.Load();
        StartupHelper.SetEnabled(_settings.StartWithWindows);

        _trayIcon = new NotifyIcon
        {
            Icon = LoadOrCreateIcon(),
            Text = TrayTooltip(),
            Visible = true,
            ContextMenuStrip = BuildMenu()
        };
        _trayIcon.DoubleClick += (_, _) => StartCapture();

        _hotkeyWindow = new HotkeyWindow();
        _hotkeyWindow.HotkeyPressed += (_, _) => StartCapture();
        _hotkeyWindow.Register(_settings.HotkeyModifiers, _settings.HotkeyVirtualKey);
    }

    private string TrayTooltip()
    {
        var combo = HotkeyTextBox.HotkeyDisplay(_settings.HotkeyModifiers, _settings.HotkeyVirtualKey);
        var t = $"Kashot — press {combo} to capture";
        return t.Length > 63 ? t[..63] : t;
    }

    private ContextMenuStrip BuildMenu()
    {
        var menu = new ContextMenuStrip();
        menu.Items.Add("Capture Screen", null, (_, _) => StartCapture());
        menu.Items.Add("-");
        menu.Items.Add("Settings…", null, (_, _) => ShowSettings());
        menu.Items.Add("-");
        menu.Items.Add("Exit", null, (_, _) => ExitApp());
        return menu;
    }

    private void ShowSettings()
    {
        _hotkeyWindow.Unregister();
        using (var form = new SettingsForm(_settings))
        {
            form.Icon = _trayIcon.Icon;
            form.ShowDialog();
        }
        _hotkeyWindow.Register(_settings.HotkeyModifiers, _settings.HotkeyVirtualKey);
        _trayIcon.Text = TrayTooltip();
    }

    private async void StartCapture()
    {
        if (_overlay is { IsDisposed: false }) return;

        _trayIcon.ContextMenuStrip?.Close();
        SendKeys.Send("{ESC}");
        SendKeys.Send("{ESC}");
        await Task.Delay(500);

        _overlay = new OverlayForm(_settings);
        _overlay.Icon = _trayIcon.Icon;
        _overlay.CaptureCompleted += (_, msg) =>
        {
            _trayIcon.BalloonTipTitle = "Kashot";
            _trayIcon.BalloonTipText = msg;
            _trayIcon.ShowBalloonTip(2000);
        };
        _overlay.FormClosed += (_, _) => _overlay = null;
        _overlay.Show();
    }

    private void ExitApp()
    {
        _trayIcon.Visible = false;
        _hotkeyWindow.Dispose();
        Application.Exit();
    }

    private static Icon LoadOrCreateIcon()
    {
        try
        {
            var exe = Environment.ProcessPath ?? Application.ExecutablePath;
            var fromExe = Icon.ExtractAssociatedIcon(exe);
            if (fromExe != null) return fromExe;
        }
        catch { }
        return DrawFallbackIcon();
    }

    private static Icon DrawFallbackIcon()
    {
        using var bmp = new Bitmap(32, 32);
        using var g = Graphics.FromImage(bmp);
        g.SmoothingMode = SmoothingMode.AntiAlias;
        g.Clear(Color.Transparent);
        using var fill = new SolidBrush(Color.FromArgb(70, 130, 230));
        g.FillRectangle(fill, 2, 6, 28, 22);
        using var pen = new Pen(Color.White, 2);
        g.DrawRectangle(pen, 2, 6, 28, 22);
        g.FillEllipse(Brushes.White, 10, 10, 12, 12);
        g.FillEllipse(fill, 13, 13, 6, 6);
        return Icon.FromHandle(bmp.GetHicon());
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            _trayIcon?.Dispose();
            _hotkeyWindow?.Dispose();
        }
        base.Dispose(disposing);
    }
}

internal class HotkeyWindow : NativeWindow, IDisposable
{
    private const int HOTKEY_ID = 9000;
    private bool _registered;

    public event EventHandler? HotkeyPressed;

    public HotkeyWindow() => CreateHandle(new CreateParams());

    public void Register(uint mods, uint vk)
    {
        if (_registered) Unregister();
        if (vk == 0) return;
        _registered = NativeMethods.RegisterHotKey(Handle, HOTKEY_ID, mods, vk);
    }

    public void Unregister()
    {
        if (!_registered) return;
        NativeMethods.UnregisterHotKey(Handle, HOTKEY_ID);
        _registered = false;
    }

    protected override void WndProc(ref Message m)
    {
        if (m.Msg == NativeMethods.WM_HOTKEY && m.WParam.ToInt32() == HOTKEY_ID)
            HotkeyPressed?.Invoke(this, EventArgs.Empty);
        base.WndProc(ref m);
    }

    public void Dispose()
    {
        Unregister();
        DestroyHandle();
    }
}
