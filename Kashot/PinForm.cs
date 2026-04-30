using System.Drawing.Imaging;

namespace Kashot;

public class PinForm : Form
{
    private readonly Bitmap _image;
    private Point _dragStart;
    private bool _dragging;

    public PinForm(Bitmap image, Point screenLocation)
    {
        _image = image;
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        TopMost = true;
        ShowInTaskbar = false;
        BackColor = Color.Magenta;
        TransparencyKey = Color.Magenta;
        ClientSize = image.Size;
        Location = screenLocation;
        Cursor = Cursors.SizeAll;
        KeyPreview = true;

        SetStyle(
            ControlStyles.AllPaintingInWmPaint |
            ControlStyles.UserPaint |
            ControlStyles.OptimizedDoubleBuffer, true);

        ContextMenuStrip = BuildMenu();
    }

    private ContextMenuStrip BuildMenu()
    {
        var menu = new ContextMenuStrip();
        menu.Items.Add("Copy", null, (_, _) => Copy());
        menu.Items.Add("Save As…", null, (_, _) => Save());
        menu.Items.Add("-");
        menu.Items.Add("Close", null, (_, _) => Close());
        return menu;
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        e.Graphics.DrawImageUnscaled(_image, 0, 0);
        using var border = new Pen(Color.FromArgb(100, 149, 237), 2);
        e.Graphics.DrawRectangle(border, 0, 0, ClientSize.Width - 1, ClientSize.Height - 1);
    }

    protected override void OnMouseDown(MouseEventArgs e)
    {
        if (e.Button == MouseButtons.Left)
        {
            _dragStart = e.Location;
            _dragging = true;
        }
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        if (!_dragging) return;
        Location = new Point(
            Location.X + e.X - _dragStart.X,
            Location.Y + e.Y - _dragStart.Y);
    }

    protected override void OnMouseUp(MouseEventArgs e)
    {
        _dragging = false;
    }

    protected override void OnMouseDoubleClick(MouseEventArgs e)
    {
        if (e.Button == MouseButtons.Left) Close();
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (e.KeyCode == Keys.Escape) Close();
        else if (e.Control && e.KeyCode == Keys.C) Copy();
        else if (e.Control && e.KeyCode == Keys.S) Save();
        base.OnKeyDown(e);
    }

    private void Copy() => Clipboard.SetImage(_image);

    private void Save()
    {
        using var dlg = new SaveFileDialog
        {
            Filter = "PNG Image|*.png|JPEG Image|*.jpg|Bitmap|*.bmp",
            DefaultExt = "png",
            FileName = $"kashot_{DateTime.Now:yyyyMMdd_HHmmss}",
        };
        if (dlg.ShowDialog() != DialogResult.OK) return;
        var fmt = Path.GetExtension(dlg.FileName).ToLower() switch
        {
            ".jpg" or ".jpeg" => ImageFormat.Jpeg,
            ".bmp" => ImageFormat.Bmp,
            _ => ImageFormat.Png,
        };
        _image.Save(dlg.FileName, fmt);
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        _image.Dispose();
        base.OnFormClosed(e);
    }
}
