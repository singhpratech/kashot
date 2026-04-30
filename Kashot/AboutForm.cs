using System.Diagnostics;
using System.Drawing.Drawing2D;

namespace Kashot;

public class AboutForm : Form
{
    public AboutForm(string theme)
    {
        var colors = ThemeColors.For(theme);

        Text = "About Kashot";
        Size = new Size(440, 380);
        FormBorderStyle = FormBorderStyle.FixedDialog;
        MaximizeBox = false;
        MinimizeBox = false;
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = colors.Background;
        ForeColor = colors.Text;
        Font = new Font("Segoe UI", 9.5f);

        var iconPanel = new Panel
        {
            Size = new Size(96, 96),
            Location = new Point((ClientSize.Width - 96) / 2, 24),
            BackColor = Color.Transparent,
        };
        iconPanel.Paint += (_, e) => DrawIcon(e.Graphics, new Rectangle(0, 0, 96, 96));

        var name = new Label
        {
            Text = "Kashot",
            Font = new Font("Segoe UI", 22f, FontStyle.Bold),
            ForeColor = colors.Text,
            BackColor = Color.Transparent,
            TextAlign = ContentAlignment.MiddleCenter,
            Location = new Point(20, 130),
            Size = new Size(ClientSize.Width - 40, 40),
        };

        var version = new Label
        {
            Text = "Version 1.0.0",
            Font = new Font("Segoe UI", 9.5f),
            ForeColor = colors.TextMuted,
            BackColor = Color.Transparent,
            TextAlign = ContentAlignment.MiddleCenter,
            Location = new Point(20, 172),
            Size = new Size(ClientSize.Width - 40, 20),
        };

        var love = new Label
        {
            Text = "With love from PrateekSingh ❤",
            Font = new Font("Segoe UI", 11f, FontStyle.Italic),
            ForeColor = colors.Accent,
            BackColor = Color.Transparent,
            TextAlign = ContentAlignment.MiddleCenter,
            Location = new Point(20, 202),
            Size = new Size(ClientSize.Width - 40, 24),
        };

        var copyright = new Label
        {
            Text = $"© {DateTime.Now.Year} PrateekSingh. All rights reserved.",
            Font = new Font("Segoe UI", 8.5f),
            ForeColor = colors.TextMuted,
            BackColor = Color.Transparent,
            TextAlign = ContentAlignment.MiddleCenter,
            Location = new Point(20, 230),
            Size = new Size(ClientSize.Width - 40, 20),
        };

        var link = new LinkLabel
        {
            Text = "github.com/singhpratech/kashot",
            Font = new Font("Segoe UI", 9f),
            BackColor = Color.Transparent,
            LinkColor = colors.Accent,
            ActiveLinkColor = colors.Accent,
            VisitedLinkColor = colors.Accent,
            TextAlign = ContentAlignment.MiddleCenter,
            Location = new Point(20, 256),
            Size = new Size(ClientSize.Width - 40, 20),
        };
        link.LinkClicked += (_, _) =>
        {
            try { Process.Start(new ProcessStartInfo("https://github.com/singhpratech/kashot") { UseShellExecute = true }); }
            catch { }
        };

        var ok = new Button
        {
            Text = "OK",
            Location = new Point((ClientSize.Width - 96) / 2, 296),
            Size = new Size(96, 30),
            DialogResult = DialogResult.OK,
            FlatStyle = FlatStyle.Flat,
            BackColor = colors.ButtonBg,
            ForeColor = colors.Text,
        };
        ok.FlatAppearance.BorderColor = colors.Border;
        ok.FlatAppearance.BorderSize = 1;
        ok.FlatAppearance.MouseOverBackColor = colors.ButtonHover;

        AcceptButton = ok;
        CancelButton = ok;

        Controls.AddRange(new Control[] { iconPanel, name, version, love, copyright, link, ok });
    }

    private static void DrawIcon(Graphics g, Rectangle r)
    {
        g.SmoothingMode = SmoothingMode.AntiAlias;
        g.PixelOffsetMode = PixelOffsetMode.HighQuality;

        int size = Math.Min(r.Width, r.Height);
        int pad = Math.Max(1, size / 24);
        int radius = Math.Max(2, size * 22 / 100);
        int w = size - 2 * pad;
        int h = size - 2 * pad;

        using var path = new GraphicsPath();
        path.AddArc(pad + r.X, pad + r.Y, radius * 2, radius * 2, 180, 90);
        path.AddArc(pad + r.X + w - radius * 2, pad + r.Y, radius * 2, radius * 2, 270, 90);
        path.AddArc(pad + r.X + w - radius * 2, pad + r.Y + h - radius * 2, radius * 2, radius * 2, 0, 90);
        path.AddArc(pad + r.X, pad + r.Y + h - radius * 2, radius * 2, radius * 2, 90, 90);
        path.CloseFigure();

        using var brush = new LinearGradientBrush(
            new Point(r.X, r.Y),
            new Point(r.X + size, r.Y + size),
            Color.FromArgb(255, 255, 255, 0),
            Color.FromArgb(255, 0, 255, 200));
        g.FillPath(brush, path);

        float bw = Math.Max(1.5f, size / 13f);
        int bl = size * 22 / 100;
        int inset = size * 22 / 100;
        using var pen = new Pen(Color.White, bw)
        {
            StartCap = LineCap.Round,
            EndCap = LineCap.Round,
        };
        int x = r.X, y = r.Y;
        g.DrawLine(pen, x + inset, y + inset, x + inset + bl, y + inset);
        g.DrawLine(pen, x + inset, y + inset, x + inset, y + inset + bl);
        g.DrawLine(pen, x + size - inset, y + inset, x + size - inset - bl, y + inset);
        g.DrawLine(pen, x + size - inset, y + inset, x + size - inset, y + inset + bl);
        g.DrawLine(pen, x + inset, y + size - inset, x + inset + bl, y + size - inset);
        g.DrawLine(pen, x + inset, y + size - inset, x + inset, y + size - inset - bl);
        g.DrawLine(pen, x + size - inset, y + size - inset, x + size - inset - bl, y + size - inset);
        g.DrawLine(pen, x + size - inset, y + size - inset, x + size - inset, y + size - inset - bl);

        int dotR = Math.Max(2, size * 9 / 100);
        int cx = r.X + size / 2, cy = r.Y + size / 2;
        g.FillEllipse(Brushes.White, cx - dotR, cy - dotR, dotR * 2, dotR * 2);
    }
}
