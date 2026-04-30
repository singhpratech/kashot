using System.Drawing.Drawing2D;

namespace Kashot;

public enum Tool { Pen, Line, Arrow, Rectangle, Ellipse, Marker, Text, Step, Pixelate, Meme }

public abstract class Annotation
{
    public Color Color { get; set; } = Color.Red;
    public float Thickness { get; set; } = 2f;
    public abstract void Draw(Graphics g);
}

public class PenAnnotation : Annotation
{
    public List<Point> Points { get; set; } = new();

    public override void Draw(Graphics g)
    {
        if (Points.Count < 2) return;
        using var pen = new Pen(Color, Thickness)
        {
            LineJoin = LineJoin.Round,
            StartCap = LineCap.Round,
            EndCap = LineCap.Round
        };
        g.DrawLines(pen, Points.ToArray());
    }
}

public class LineAnnotation : Annotation
{
    public Point Start { get; set; }
    public Point End { get; set; }

    public override void Draw(Graphics g)
    {
        using var pen = new Pen(Color, Thickness)
        {
            StartCap = LineCap.Round,
            EndCap = LineCap.Round
        };
        g.DrawLine(pen, Start, End);
    }
}

public class ArrowAnnotation : Annotation
{
    public Point Start { get; set; }
    public Point End { get; set; }

    public override void Draw(Graphics g)
    {
        // CustomLineCap implements IDisposable but a Pen does NOT dispose its
        // CustomEndCap when the pen disposes — without this `using` we leak one
        // GDI handle every redraw, which kills long-running tray sessions.
        using var cap = new AdjustableArrowCap(Thickness + 3, Thickness + 3);
        using var pen = new Pen(Color, Thickness)
        {
            StartCap      = LineCap.Round,
            CustomEndCap  = cap,
        };
        g.DrawLine(pen, Start, End);
    }
}

public class RectAnnotation : Annotation
{
    public Point Start { get; set; }
    public Point End { get; set; }

    public Rectangle GetRect()
    {
        int x = Math.Min(Start.X, End.X);
        int y = Math.Min(Start.Y, End.Y);
        return new Rectangle(x, y, Math.Abs(End.X - Start.X), Math.Abs(End.Y - Start.Y));
    }

    public override void Draw(Graphics g)
    {
        var rect = GetRect();
        if (rect.Width == 0 || rect.Height == 0) return;
        using var pen = new Pen(Color, Thickness);
        g.DrawRectangle(pen, rect);
    }
}

public class EllipseAnnotation : Annotation
{
    public Point Start { get; set; }
    public Point End { get; set; }

    public Rectangle GetRect()
    {
        int x = Math.Min(Start.X, End.X);
        int y = Math.Min(Start.Y, End.Y);
        return new Rectangle(x, y, Math.Abs(End.X - Start.X), Math.Abs(End.Y - Start.Y));
    }

    public override void Draw(Graphics g)
    {
        var rect = GetRect();
        if (rect.Width == 0 || rect.Height == 0) return;
        using var pen = new Pen(Color, Thickness);
        g.DrawEllipse(pen, rect);
    }
}

public class MarkerAnnotation : Annotation
{
    public List<Point> Points { get; set; } = new();

    public override void Draw(Graphics g)
    {
        if (Points.Count < 2) return;
        using var pen = new Pen(Color.FromArgb(80, Color), Thickness)
        {
            LineJoin = LineJoin.Round,
            StartCap = LineCap.Round,
            EndCap = LineCap.Round
        };
        g.DrawLines(pen, Points.ToArray());
    }
}

public class TextAnnotation : Annotation
{
    // Shared default font — we used to allocate a new Font per annotation,
    // which leaked GDI font handles forever (Annotations don't own a
    // Dispose path). Callers that genuinely need a custom font can still
    // assign `TextFont`, but they're then responsible for disposing it.
    private static readonly Font DefaultFont = new("Segoe UI", 14f, FontStyle.Bold);

    public Point  Position { get; set; }
    public string Text     { get; set; } = "";
    public Font   TextFont { get; set; } = DefaultFont;

    public override void Draw(Graphics g)
    {
        if (string.IsNullOrEmpty(Text)) return;
        using var brush  = new SolidBrush(Color);
        using var shadow = new SolidBrush(Color.FromArgb(60, 0, 0, 0));
        g.DrawString(Text, TextFont, shadow, Position.X + 1, Position.Y + 1);
        g.DrawString(Text, TextFont, brush,  Position);
    }
}

public class StepAnnotation : Annotation
{
    public Point Center { get; set; }
    public int Number { get; set; } = 1;

    public override void Draw(Graphics g)
    {
        const int radius = 14;
        var rect = new Rectangle(Center.X - radius, Center.Y - radius, radius * 2, radius * 2);
        using var fill = new SolidBrush(Color);
        using var border = new Pen(Color.White, 2);
        g.FillEllipse(fill, rect);
        g.DrawEllipse(border, rect);

        using var f = new Font("Segoe UI", 11f, FontStyle.Bold);
        using var sf = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center };
        g.DrawString(Number.ToString(), f, Brushes.White, rect, sf);
    }
}

public class MemeAnnotation : Annotation
{
    public Point Position { get; set; }
    public string Text { get; set; } = "";
    public float FontSize { get; set; } = 36f;

    public override void Draw(Graphics g)
    {
        if (string.IsNullOrWhiteSpace(Text)) return;

        FontFamily? family = null;
        foreach (var name in new[] { "Impact", "Anton", "Arial Black", "Segoe UI Black", "Segoe UI" })
        {
            try { family = new FontFamily(name); break; }
            catch { }
        }
        family ??= FontFamily.GenericSansSerif;

        using var path = new GraphicsPath();
        path.AddString(Text.ToUpperInvariant(), family, (int)FontStyle.Bold,
            FontSize, Position, StringFormat.GenericTypographic);

        var oldSmoothing = g.SmoothingMode;
        g.SmoothingMode = SmoothingMode.AntiAlias;

        using var outline = new Pen(Color.Black, Math.Max(3f, FontSize / 9f))
        {
            LineJoin = LineJoin.Round,
        };
        g.DrawPath(outline, path);
        using var fill = new SolidBrush(Color.White);
        g.FillPath(fill, path);

        g.SmoothingMode = oldSmoothing;
    }
}

public class PixelateAnnotation : Annotation
{
    public Point Start { get; set; }
    public Point End { get; set; }
    public Bitmap? Source { get; set; }
    public int BlockSize { get; set; } = 10;

    public Rectangle GetRect()
    {
        int x = Math.Min(Start.X, End.X);
        int y = Math.Min(Start.Y, End.Y);
        return new Rectangle(x, y, Math.Abs(End.X - Start.X), Math.Abs(End.Y - Start.Y));
    }

    public override void Draw(Graphics g)
    {
        var rect = GetRect();
        if (rect.Width <= 0 || rect.Height <= 0 || Source == null) return;

        var src = Rectangle.Intersect(rect, new Rectangle(0, 0, Source.Width, Source.Height));
        if (src.Width <= 0 || src.Height <= 0) return;

        int smallW = Math.Max(1, src.Width / BlockSize);
        int smallH = Math.Max(1, src.Height / BlockSize);

        using var small = new Bitmap(smallW, smallH);
        using (var sg = Graphics.FromImage(small))
        {
            sg.InterpolationMode = InterpolationMode.HighQualityBilinear;
            sg.PixelOffsetMode = PixelOffsetMode.HighQuality;
            sg.DrawImage(Source, new Rectangle(0, 0, smallW, smallH), src, GraphicsUnit.Pixel);
        }

        var oldInterp = g.InterpolationMode;
        var oldOffset = g.PixelOffsetMode;
        g.InterpolationMode = InterpolationMode.NearestNeighbor;
        g.PixelOffsetMode = PixelOffsetMode.Half;
        g.DrawImage(small, src);
        g.InterpolationMode = oldInterp;
        g.PixelOffsetMode = oldOffset;
    }
}
