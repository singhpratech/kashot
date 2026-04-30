using System.Drawing.Drawing2D;

namespace Kashot;

public class TrayContext : ApplicationContext
{
    private readonly AppSettings _settings;
    private readonly NotifyIcon _trayIcon;
    private readonly HotkeyWindow _hotkeyWindow;
    private OverlayForm? _overlay;
    private KashotRecorder? _recorder;
    private ToolStripMenuItem? _recordItem;
    private ToolStripMenuItem? _stopRecordItem;

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
        // Modern Windows accepts NotifyIcon tooltips up to 127 chars. Older
        // versions truncate at 63 themselves — let the OS decide rather than
        // chopping mid-word here. The user-visible label stays whole.
        return $"Kashot — press {combo} to capture";
    }

    private ContextMenuStrip BuildMenu()
    {
        var menu = new ContextMenuStrip();
        menu.Items.Add("Capture Screen",        null, (_, _) => StartCapture());

        // "Capture after delay…" submenu — three preset durations covering
        // the common screenshot-tool delay use cases (open a menu, focus a
        // window, dismiss a tooltip, etc.) without a free-form input UI.
        var delay = new ToolStripMenuItem("Capture after delay");
        delay.DropDownItems.Add("3 seconds",    null, (_, _) => StartCaptureAfter(3));
        delay.DropDownItems.Add("5 seconds",    null, (_, _) => StartCaptureAfter(5));
        delay.DropDownItems.Add("10 seconds",   null, (_, _) => StartCaptureAfter(10));
        menu.Items.Add(delay);

        menu.Items.Add("-");
        _recordItem     = new ToolStripMenuItem("Record Screen", null, (_, _) => StartRecording())
                          { Enabled = true };
        _stopRecordItem = new ToolStripMenuItem("Stop Recording", null, (_, _) => StopRecording())
                          { Enabled = false };
        menu.Items.Add(_recordItem);
        menu.Items.Add(_stopRecordItem);

        menu.Items.Add("-");
        menu.Items.Add("Open Save Folder",      null, (_, _) => OpenSaveFolder());
        menu.Items.Add("-");
        menu.Items.Add("Settings…",             null, (_, _) => ShowSettings());
        menu.Items.Add("About",                 null, (_, _) => ShowAbout());
        menu.Items.Add("Check for updates",     null, (_, _) => OpenReleasesPage());
        menu.Items.Add("-");
        menu.Items.Add("Exit",                  null, (_, _) => ExitApp());
        return menu;
    }

    private void OpenReleasesPage()
    {
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(
                "https://github.com/singhpratech/kashot/releases")
            { UseShellExecute = true });
        }
        catch (Exception ex)
        {
            _trayIcon.BalloonTipTitle = "Kashot";
            _trayIcon.BalloonTipText  = $"Couldn't open browser: {ex.Message}";
            _trayIcon.ShowBalloonTip(3000);
        }
    }

    /// Begin recording the primary display to MP4 in the user's Videos
    /// folder. Wraps the existing KashotRecorder; UI state tracks
    /// recording so the menu shows only one of Record / Stop at a time.
    private void StartRecording()
    {
        if (_recorder?.IsRecording == true) return;
        _trayIcon.ContextMenuStrip?.Close();

        _recorder ??= NewRecorder();

        var dir = Environment.GetFolderPath(Environment.SpecialFolder.MyVideos);
        if (string.IsNullOrWhiteSpace(dir)) dir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        Directory.CreateDirectory(dir);
        var stamp = DateTime.Now.ToString("yyyyMMdd_HHmmss");
        var path  = Path.Combine(dir, $"kashot_{stamp}.mp4");

        try
        {
            _recorder.Start(path, micEnabled: false, systemAudioEnabled: false);
            SetRecordingUi(true);
            _trayIcon.BalloonTipTitle = "Kashot";
            _trayIcon.BalloonTipText  = $"Recording → {Path.GetFileName(path)}";
            _trayIcon.ShowBalloonTip(2000);
        }
        catch (Exception ex)
        {
            SetRecordingUi(false);
            _trayIcon.BalloonTipTitle = "Kashot — recording failed";
            _trayIcon.BalloonTipText  = ex.Message;
            _trayIcon.ShowBalloonTip(4000);
        }
    }

    private void StopRecording()
    {
        if (_recorder?.IsRecording != true) return;
        _trayIcon.ContextMenuStrip?.Close();
        _recorder.Stop();
        // UI flips on RecordingComplete; nothing more to do here.
    }

    private KashotRecorder NewRecorder()
    {
        var r = new KashotRecorder();
        r.RecordingComplete += path =>
        {
            // Marshalling: tray-icon callbacks fire on the recorder thread.
            // BeginInvoke onto the UI thread via the tray icon's underlying
            // form/sync context isn't trivial here — but BalloonTipText is
            // safe to set from any thread, and ShowBalloonTip self-marshals.
            SetRecordingUi(false);
            _trayIcon.BalloonTipTitle = "Kashot";
            _trayIcon.BalloonTipText  = $"Saved {Path.GetFileName(path)}";
            _trayIcon.ShowBalloonTip(3000);
        };
        r.RecordingFailed += err =>
        {
            SetRecordingUi(false);
            _trayIcon.BalloonTipTitle = "Kashot — recording failed";
            _trayIcon.BalloonTipText  = err.Length > 200 ? err[..200] : err;
            _trayIcon.ShowBalloonTip(4000);
        };
        return r;
    }

    private void SetRecordingUi(bool recording)
    {
        if (_recordItem     != null) _recordItem.Enabled     = !recording;
        if (_stopRecordItem != null) _stopRecordItem.Enabled =  recording;
    }

    /// Schedule a capture after the given delay, with a balloon countdown so
    /// the user knows the timer is running. Same StartCapture() codepath
    /// otherwise — the overlay editor opens when the timer fires.
    private async void StartCaptureAfter(int seconds)
    {
        if (_overlay is { IsDisposed: false }) return;

        _trayIcon.ContextMenuStrip?.Close();
        _trayIcon.BalloonTipTitle = "Kashot";
        _trayIcon.BalloonTipText  = $"Capturing in {seconds} second{(seconds == 1 ? "" : "s")}…";
        _trayIcon.ShowBalloonTip(seconds * 1000);

        try { await Task.Delay(seconds * 1000); }
        catch (Exception) { return; }

        StartCapture();
    }

    private void ShowAbout()
    {
        using var about = new AboutForm(_settings.Theme);
        about.Icon = _trayIcon.Icon;
        about.ShowDialog();
    }

    private void OpenSaveFolder()
    {
        var path = !string.IsNullOrWhiteSpace(_settings.SaveDirectory) && Directory.Exists(_settings.SaveDirectory)
            ? _settings.SaveDirectory
            : Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(path)
            {
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            _trayIcon.BalloonTipTitle = "Kashot";
            _trayIcon.BalloonTipText  = $"Couldn't open folder: {ex.Message}";
            _trayIcon.ShowBalloonTip(3000);
        }
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

        // Capture / overlay construction can fail in unusual environments
        // (locked workstation, RDP session with no console, GDI exhaustion).
        // Show a balloon and bail instead of taking the whole tray app down.
        try
        {
            _overlay = new OverlayForm(_settings);
            _overlay.Icon = _trayIcon.Icon;
            _overlay.CaptureCompleted += (_, msg) =>
            {
                _trayIcon.BalloonTipTitle = "Kashot";
                _trayIcon.BalloonTipText  = msg;
                _trayIcon.ShowBalloonTip(2000);
            };
            _overlay.FormClosed += (_, _) => _overlay = null;
            _overlay.Show();
        }
        catch (Exception ex)
        {
            _overlay = null;
            _trayIcon.BalloonTipTitle = "Kashot — capture failed";
            _trayIcon.BalloonTipText  = ex.Message.Length > 200 ? ex.Message[..200] : ex.Message;
            _trayIcon.ShowBalloonTip(4000);
        }
    }

    private void ExitApp()
    {
        // Stop any in-flight recording so the MP4 finalizes before we tear
        // down the process. The OnRecordingComplete callback may not fire
        // if we Application.Exit() too quickly, but the file on disk should
        // already be playable from KashotRecorder.Stop.
        if (_recorder?.IsRecording == true)
        {
            try { _recorder.Stop(); } catch { /* swallow */ }
        }
        _recorder?.Dispose();
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
            _recorder?.Dispose();
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
