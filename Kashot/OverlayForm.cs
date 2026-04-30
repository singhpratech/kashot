using System.Drawing.Drawing2D;
using System.Drawing.Imaging;

namespace Kashot;

public class OverlayForm : Form
{
    private enum State { Idle, Selecting, Selected, Drawing, TextInput, Resizing, Moving }
    private enum Edge { None, Left, Right, Top, Bottom, TopLeft, TopRight, BottomLeft, BottomRight }

    public event EventHandler<string>? CaptureCompleted;

    private readonly AppSettings _settings;

    private State _state = State.Idle;
    private Tool _tool = Tool.Pen;
    private Color _color = Color.Red;
    private float _thickness = 3f;

    private readonly Bitmap _screenshot;
    private readonly Rectangle _virtualBounds;

    private Point _selStart;
    private Point _selCurrent;
    private Rectangle _selection;

    private Edge _resizeEdge = Edge.None;
    private Rectangle _selStartRect;

    private readonly List<Annotation> _annotations = new();
    private readonly List<Annotation> _redoStack = new();
    private Annotation? _active;

    private Panel? _toolPanel;
    private Panel? _actionPanel;
    private Panel? _colorPopup;
    private TextBox? _textBox;
    private readonly List<Button> _toolButtons = new();
    private Button? _colorBtn;

    private const int EdgeThreshold = 8;
    private int _paletteIndex;

    private record ColorPalette(string Name, Color[] Colors);

    private static readonly ColorPalette[] Palettes =
    {
        new("Vivid", new[]
        {
            Color.FromArgb(255, 220, 38, 38),
            Color.FromArgb(255, 255, 100, 0),
            Color.FromArgb(255, 255, 180, 0),
            Color.FromArgb(255, 255, 230, 0),
            Color.FromArgb(255, 130, 220, 0),
            Color.FromArgb(255, 0, 180, 80),
            Color.FromArgb(255, 0, 200, 200),
            Color.FromArgb(255, 0, 180, 240),
            Color.FromArgb(255, 0, 100, 255),
            Color.FromArgb(255, 80, 80, 220),
            Color.FromArgb(255, 160, 60, 240),
            Color.FromArgb(255, 240, 60, 220),
            Color.FromArgb(255, 255, 100, 200),
            Color.FromArgb(255, 255, 80, 80),
            Color.FromArgb(255, 255, 255, 255),
            Color.FromArgb(255, 0, 0, 0),
        }),
        new("Highlighter", new[]
        {
            Color.FromArgb(170, 255, 235, 0),
            Color.FromArgb(170, 100, 255, 100),
            Color.FromArgb(170, 255, 100, 200),
            Color.FromArgb(170, 100, 230, 255),
            Color.FromArgb(170, 255, 150, 0),
            Color.FromArgb(170, 200, 100, 255),
            Color.FromArgb(170, 50, 220, 50),
            Color.FromArgb(170, 100, 150, 255),
            Color.FromArgb(170, 255, 80, 80),
            Color.FromArgb(170, 240, 60, 220),
            Color.FromArgb(170, 0, 200, 200),
            Color.FromArgb(170, 255, 180, 100),
            Color.FromArgb(170, 180, 255, 100),
            Color.FromArgb(170, 100, 100, 255),
            Color.FromArgb(170, 230, 230, 230),
            Color.FromArgb(170, 80, 80, 80),
        }),
        new("Pastel", new[]
        {
            Color.FromArgb(255, 255, 200, 200),
            Color.FromArgb(255, 255, 220, 180),
            Color.FromArgb(255, 255, 235, 180),
            Color.FromArgb(255, 255, 245, 200),
            Color.FromArgb(255, 230, 255, 200),
            Color.FromArgb(255, 200, 255, 220),
            Color.FromArgb(255, 200, 245, 245),
            Color.FromArgb(255, 200, 230, 255),
            Color.FromArgb(255, 200, 215, 255),
            Color.FromArgb(255, 220, 200, 255),
            Color.FromArgb(255, 240, 200, 255),
            Color.FromArgb(255, 255, 200, 235),
            Color.FromArgb(255, 255, 215, 215),
            Color.FromArgb(255, 240, 240, 240),
            Color.FromArgb(255, 200, 200, 210),
            Color.FromArgb(255, 100, 100, 110),
        }),
        new("Pro", new[]
        {
            Color.FromArgb(255, 220, 38, 38),
            Color.FromArgb(255, 30, 100, 220),
            Color.FromArgb(255, 30, 160, 60),
            Color.FromArgb(255, 240, 120, 0),
            Color.FromArgb(255, 138, 43, 226),
            Color.FromArgb(255, 220, 180, 0),
            Color.FromArgb(255, 0, 160, 200),
            Color.FromArgb(255, 200, 60, 130),
            Color.FromArgb(255, 100, 30, 30),
            Color.FromArgb(255, 30, 30, 100),
            Color.FromArgb(255, 30, 80, 30),
            Color.FromArgb(255, 100, 60, 0),
            Color.FromArgb(255, 0, 0, 0),
            Color.FromArgb(255, 80, 80, 80),
            Color.FromArgb(255, 200, 200, 200),
            Color.FromArgb(255, 255, 255, 255),
        }),
    };

    public OverlayForm(AppSettings settings)
    {
        _settings = settings;
        _tool = ParseTool(_settings.LastTool);
        _color = _settings.LastColor;
        _thickness = _settings.LastThickness;
        _paletteIndex = Math.Clamp(_settings.PaletteIndex, 0, Palettes.Length - 1);

        _virtualBounds = SystemInformation.VirtualScreen;
        _screenshot = CaptureScreen();

        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Location = _virtualBounds.Location;
        Size = _virtualBounds.Size;
        TopMost = true;
        ShowInTaskbar = false;
        KeyPreview = true;
        Cursor = Cursors.Cross;

        SetStyle(
            ControlStyles.AllPaintingInWmPaint |
            ControlStyles.UserPaint |
            ControlStyles.OptimizedDoubleBuffer, true);
    }

    private static Tool ParseTool(string s) =>
        Enum.TryParse<Tool>(s, ignoreCase: true, out var t) ? t : Tool.Pen;

    private Bitmap CaptureScreen()
    {
        var bmp = new Bitmap(_virtualBounds.Width, _virtualBounds.Height, PixelFormat.Format32bppArgb);
        using var g = Graphics.FromImage(bmp);
        g.CopyFromScreen(_virtualBounds.Location, Point.Empty, _virtualBounds.Size);
        return bmp;
    }

    // ═══════════════════════════════════════════
    //  PAINTING
    // ═══════════════════════════════════════════

    protected override void OnPaintBackground(PaintEventArgs e) { }

    protected override void OnPaint(PaintEventArgs e)
    {
        var g = e.Graphics;
        g.SmoothingMode = SmoothingMode.AntiAlias;

        g.DrawImageUnscaled(_screenshot, 0, 0);

        using (var dim = new SolidBrush(Color.FromArgb(100, 0, 0, 0)))
            g.FillRectangle(dim, ClientRectangle);

        var sel = CurrentSelection();

        if (sel.Width > 0 && sel.Height > 0)
        {
            g.DrawImage(_screenshot, sel, sel, GraphicsUnit.Pixel);

            using (var bp = new Pen(Color.FromArgb(100, 149, 237), 1) { DashStyle = DashStyle.Dash })
                g.DrawRectangle(bp, sel);

            var clip = g.ClipBounds;
            g.SetClip(sel);
            foreach (var a in _annotations) a.Draw(g);
            _active?.Draw(g);
            g.SetClip(new RectangleF(clip.X, clip.Y, clip.Width, clip.Height));

            if (_state == State.Selected || _state == State.Resizing || _state == State.Moving)
                DrawResizeHandles(g, sel);

            DrawDimLabel(g, sel);
        }

        if (_state is State.Idle or State.Selecting)
        {
            var m = PointToClient(Cursor.Position);
            DrawCrosshair(g, m);
            DrawMagnifier(g, m);
        }
    }

    private Rectangle CurrentSelection()
    {
        if (_state == State.Idle) return Rectangle.Empty;
        if (_state == State.Selecting)
            return Normalize(_selStart, _selCurrent);
        return _selection;
    }

    private static Rectangle Normalize(Point a, Point b) =>
        new(Math.Min(a.X, b.X), Math.Min(a.Y, b.Y), Math.Abs(b.X - a.X), Math.Abs(b.Y - a.Y));

    private void DrawCrosshair(Graphics g, Point p)
    {
        using var pen = new Pen(Color.FromArgb(150, 100, 149, 237), 1) { DashStyle = DashStyle.Dot };
        g.DrawLine(pen, 0, p.Y, ClientSize.Width, p.Y);
        g.DrawLine(pen, p.X, 0, p.X, ClientSize.Height);
    }

    private void DrawMagnifier(Graphics g, Point p)
    {
        const int src = 30, zoom = 4, mag = src * zoom;

        int mx = p.X + 25, my = p.Y + 25;
        if (mx + mag + 2 > ClientSize.Width) mx = p.X - mag - 25;
        if (my + mag + 30 > ClientSize.Height) my = p.Y - mag - 45;

        var srcX = Math.Clamp(p.X - src / 2, 0, Math.Max(0, _screenshot.Width - src));
        var srcY = Math.Clamp(p.Y - src / 2, 0, Math.Max(0, _screenshot.Height - src));

        g.FillRectangle(Brushes.Black, mx - 1, my - 1, mag + 2, mag + 22);

        var oldInterp = g.InterpolationMode;
        var oldOffset = g.PixelOffsetMode;
        g.InterpolationMode = InterpolationMode.NearestNeighbor;
        g.PixelOffsetMode = PixelOffsetMode.Half;
        g.DrawImage(_screenshot,
            new Rectangle(mx, my, mag, mag),
            new Rectangle(srcX, srcY, src, src),
            GraphicsUnit.Pixel);
        g.InterpolationMode = oldInterp;
        g.PixelOffsetMode = oldOffset;

        using var cp = new Pen(Color.FromArgb(180, 255, 255, 255), 1);
        g.DrawLine(cp, mx, my + mag / 2, mx + mag, my + mag / 2);
        g.DrawLine(cp, mx + mag / 2, my, mx + mag / 2, my + mag);

        using var bp = new Pen(Color.FromArgb(100, 149, 237), 2);
        g.DrawRectangle(bp, mx, my, mag, mag);

        using var f = new Font("Consolas", 8f);
        g.DrawString($"X:{p.X + _virtualBounds.X} Y:{p.Y + _virtualBounds.Y}", f, Brushes.White, mx, my + mag + 3);
    }

    private void DrawDimLabel(Graphics g, Rectangle sel)
    {
        var txt = $"{sel.Width} x {sel.Height}";
        using var f = new Font("Segoe UI", 9f);
        var sz = g.MeasureString(txt, f);
        float lx = sel.X, ly = sel.Bottom + 4;
        if (ly + sz.Height > ClientSize.Height) ly = sel.Y - sz.Height - 4;

        using (var bg = new SolidBrush(Color.FromArgb(200, 30, 30, 30)))
            g.FillRectangle(bg, lx, ly, sz.Width + 8, sz.Height + 4);
        g.DrawString(txt, f, Brushes.White, lx + 4, ly + 2);
    }

    private static void DrawResizeHandles(Graphics g, Rectangle sel)
    {
        const int s = 6;
        Point[] pts =
        {
            new(sel.Left, sel.Top), new(sel.Right, sel.Top),
            new(sel.Left, sel.Bottom), new(sel.Right, sel.Bottom),
            new(sel.Left + sel.Width / 2, sel.Top), new(sel.Left + sel.Width / 2, sel.Bottom),
            new(sel.Left, sel.Top + sel.Height / 2), new(sel.Right, sel.Top + sel.Height / 2),
        };
        using var fill = new SolidBrush(Color.White);
        using var border = new Pen(Color.FromArgb(100, 149, 237), 1);
        foreach (var p in pts)
        {
            var r = new Rectangle(p.X - s / 2, p.Y - s / 2, s, s);
            g.FillRectangle(fill, r);
            g.DrawRectangle(border, r);
        }
    }

    // ═══════════════════════════════════════════
    //  MOUSE
    // ═══════════════════════════════════════════

    protected override void OnMouseDown(MouseEventArgs e)
    {
        DismissColorPopup();

        if (e.Button == MouseButtons.Right)
        {
            HandleRightClick();
            return;
        }
        if (e.Button != MouseButtons.Left) return;

        switch (_state)
        {
            case State.Idle:
                _selStart = _selCurrent = e.Location;
                _state = State.Selecting;
                break;

            case State.Selected:
                var edge = HitTestEdge(e.Location);
                if (edge != Edge.None)
                {
                    _resizeEdge = edge;
                    _selStart = e.Location;
                    _selStartRect = _selection;
                    _state = State.Resizing;
                }
                else if ((ModifierKeys & Keys.Alt) == Keys.Alt && _selection.Contains(e.Location))
                {
                    _selStart = e.Location;
                    _selStartRect = _selection;
                    _state = State.Moving;
                }
                else if (_selection.Contains(e.Location))
                {
                    if (_tool == Tool.Text)
                        StartTextInput(e.Location);
                    else
                        StartDrawing(e.Location);
                }
                else
                {
                    HideToolbars();
                    _annotations.Clear();
                    _redoStack.Clear();
                    _selStart = _selCurrent = e.Location;
                    _state = State.Selecting;
                }
                break;
        }
        Invalidate();
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        switch (_state)
        {
            case State.Idle:
                Invalidate();
                break;
            case State.Selecting:
                _selCurrent = e.Location;
                Invalidate();
                break;
            case State.Drawing:
                UpdateDrawing(e.Location);
                Invalidate();
                break;
            case State.Selected:
                UpdateSelectedCursor(e.Location);
                break;
            case State.Resizing:
                UpdateResize(e.Location);
                Invalidate();
                break;
            case State.Moving:
                UpdateMove(e.Location);
                Invalidate();
                break;
        }
    }

    protected override void OnMouseUp(MouseEventArgs e)
    {
        if (e.Button != MouseButtons.Left) return;

        switch (_state)
        {
            case State.Selecting: FinalizeSelection(); break;
            case State.Drawing: FinalizeDrawing(); break;
            case State.Resizing:
            case State.Moving:
                _resizeEdge = Edge.None;
                _state = State.Selected;
                PositionToolbars();
                Invalidate();
                break;
        }
    }

    private void UpdateSelectedCursor(Point p)
    {
        var edge = HitTestEdge(p);
        Cursor = edge switch
        {
            Edge.Left or Edge.Right => Cursors.SizeWE,
            Edge.Top or Edge.Bottom => Cursors.SizeNS,
            Edge.TopLeft or Edge.BottomRight => Cursors.SizeNWSE,
            Edge.TopRight or Edge.BottomLeft => Cursors.SizeNESW,
            _ => _selection.Contains(p) ? Cursors.Cross : Cursors.Arrow,
        };
    }

    private Edge HitTestEdge(Point p)
    {
        if (_selection.Width == 0 || _selection.Height == 0) return Edge.None;

        bool nearLeft = Math.Abs(p.X - _selection.Left) <= EdgeThreshold;
        bool nearRight = Math.Abs(p.X - _selection.Right) <= EdgeThreshold;
        bool nearTop = Math.Abs(p.Y - _selection.Top) <= EdgeThreshold;
        bool nearBottom = Math.Abs(p.Y - _selection.Bottom) <= EdgeThreshold;
        bool inX = p.X >= _selection.Left - EdgeThreshold && p.X <= _selection.Right + EdgeThreshold;
        bool inY = p.Y >= _selection.Top - EdgeThreshold && p.Y <= _selection.Bottom + EdgeThreshold;

        if (!inX || !inY) return Edge.None;

        if (nearLeft && nearTop) return Edge.TopLeft;
        if (nearRight && nearTop) return Edge.TopRight;
        if (nearLeft && nearBottom) return Edge.BottomLeft;
        if (nearRight && nearBottom) return Edge.BottomRight;
        if (nearLeft) return Edge.Left;
        if (nearRight) return Edge.Right;
        if (nearTop) return Edge.Top;
        if (nearBottom) return Edge.Bottom;
        return Edge.None;
    }

    private void UpdateResize(Point p)
    {
        var s = _selStartRect;
        int left = s.Left, top = s.Top, right = s.Right, bottom = s.Bottom;
        int dx = p.X - _selStart.X, dy = p.Y - _selStart.Y;

        switch (_resizeEdge)
        {
            case Edge.Left: left = s.Left + dx; break;
            case Edge.Right: right = s.Right + dx; break;
            case Edge.Top: top = s.Top + dy; break;
            case Edge.Bottom: bottom = s.Bottom + dy; break;
            case Edge.TopLeft: left = s.Left + dx; top = s.Top + dy; break;
            case Edge.TopRight: right = s.Right + dx; top = s.Top + dy; break;
            case Edge.BottomLeft: left = s.Left + dx; bottom = s.Bottom + dy; break;
            case Edge.BottomRight: right = s.Right + dx; bottom = s.Bottom + dy; break;
        }

        if (right < left) (left, right) = (right, left);
        if (bottom < top) (top, bottom) = (bottom, top);
        _selection = new Rectangle(left, top, right - left, bottom - top);
        PositionToolbars();
    }

    private void UpdateMove(Point p)
    {
        int dx = p.X - _selStart.X, dy = p.Y - _selStart.Y;
        _selection = new Rectangle(
            _selStartRect.X + dx,
            _selStartRect.Y + dy,
            _selStartRect.Width,
            _selStartRect.Height);
        PositionToolbars();
    }

    private void HandleRightClick()
    {
        switch (_state)
        {
            case State.Drawing:
                _active = null;
                _state = State.Selected;
                Invalidate();
                break;
            case State.TextInput:
                CancelTextInput();
                break;
            default:
                Close();
                break;
        }
    }

    // ═══════════════════════════════════════════
    //  SELECTION
    // ═══════════════════════════════════════════

    private void FinalizeSelection()
    {
        var sel = CurrentSelection();
        if (sel.Width < 5 || sel.Height < 5)
        {
            _state = State.Idle;
            Invalidate();
            return;
        }

        _selection = sel;
        _state = State.Selected;
        ShowToolbars();
        Invalidate();
    }

    // ═══════════════════════════════════════════
    //  DRAWING
    // ═══════════════════════════════════════════

    private void StartDrawing(Point pos)
    {
        if (_tool == Tool.Step)
        {
            int n = _annotations.OfType<StepAnnotation>().Count() + 1;
            AddAnnotation(new StepAnnotation { Color = _color, Center = pos, Number = n });
            return;
        }

        _state = State.Drawing;
        _active = _tool switch
        {
            Tool.Pen => new PenAnnotation { Color = _color, Thickness = _thickness, Points = new() { pos } },
            Tool.Line => new LineAnnotation { Color = _color, Thickness = _thickness, Start = pos, End = pos },
            Tool.Arrow => new ArrowAnnotation { Color = _color, Thickness = _thickness, Start = pos, End = pos },
            Tool.Rectangle => new RectAnnotation { Color = _color, Thickness = _thickness, Start = pos, End = pos },
            Tool.Ellipse => new EllipseAnnotation { Color = _color, Thickness = _thickness, Start = pos, End = pos },
            Tool.Marker => new MarkerAnnotation { Color = _color, Thickness = _thickness * 6, Points = new() { pos } },
            Tool.Pixelate => new PixelateAnnotation { Source = _screenshot, Start = pos, End = pos },
            _ => null
        };
        if (_active == null) _state = State.Selected;
    }

    private void UpdateDrawing(Point pos)
    {
        switch (_active)
        {
            case PenAnnotation pen: pen.Points.Add(pos); break;
            case LineAnnotation line: line.End = pos; break;
            case ArrowAnnotation arrow: arrow.End = pos; break;
            case RectAnnotation rect: rect.End = pos; break;
            case EllipseAnnotation el: el.End = pos; break;
            case MarkerAnnotation marker: marker.Points.Add(pos); break;
            case PixelateAnnotation pix: pix.End = pos; break;
        }
    }

    private void FinalizeDrawing()
    {
        if (_active != null)
        {
            _annotations.Add(_active);
            _redoStack.Clear();
        }
        _active = null;
        _state = State.Selected;
        Invalidate();
    }

    private void AddAnnotation(Annotation a)
    {
        _annotations.Add(a);
        _redoStack.Clear();
        Invalidate();
    }

    private void Undo()
    {
        if (_annotations.Count == 0) return;
        var last = _annotations[^1];
        _annotations.RemoveAt(_annotations.Count - 1);
        _redoStack.Add(last);
        Invalidate();
    }

    private void Redo()
    {
        if (_redoStack.Count == 0) return;
        var item = _redoStack[^1];
        _redoStack.RemoveAt(_redoStack.Count - 1);
        _annotations.Add(item);
        Invalidate();
    }

    // ═══════════════════════════════════════════
    //  TEXT INPUT
    // ═══════════════════════════════════════════

    private void StartTextInput(Point pos)
    {
        _state = State.TextInput;
        _textBox = new TextBox
        {
            Location = pos,
            Font = new Font("Segoe UI", 14f, FontStyle.Bold),
            ForeColor = _color,
            BackColor = Color.FromArgb(240, 240, 240),
            BorderStyle = BorderStyle.FixedSingle,
            Width = Math.Min(250, _selection.Right - pos.X),
        };
        _textBox.KeyDown += (_, ke) =>
        {
            if (ke.KeyCode == Keys.Enter) { FinalizeText(); ke.SuppressKeyPress = true; }
            else if (ke.KeyCode == Keys.Escape) { CancelTextInput(); ke.SuppressKeyPress = true; }
        };
        _textBox.LostFocus += (_, _) => FinalizeText();
        Controls.Add(_textBox);
        _textBox.BringToFront();
        _textBox.Focus();
    }

    private void FinalizeText()
    {
        if (_textBox == null) return;
        if (!string.IsNullOrWhiteSpace(_textBox.Text))
        {
            AddAnnotation(new TextAnnotation
            {
                Color = _color,
                Position = _textBox.Location,
                Text = _textBox.Text
            });
        }
        RemoveTextBox();
    }

    private void CancelTextInput() => RemoveTextBox();

    private void RemoveTextBox()
    {
        if (_textBox == null) return;
        Controls.Remove(_textBox);
        _textBox.Dispose();
        _textBox = null;
        _state = State.Selected;
        Invalidate();
    }

    // ═══════════════════════════════════════════
    //  KEYBOARD
    // ═══════════════════════════════════════════

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (e.KeyCode == Keys.Escape)
        {
            if (_state == State.TextInput) CancelTextInput();
            else if (_state == State.Drawing) { _active = null; _state = State.Selected; Invalidate(); }
            else Close();
            e.Handled = true;
            return;
        }

        if (e.Control && e.KeyCode == Keys.Z)
        {
            Undo();
            e.Handled = true;
            return;
        }

        if (e.Control && (e.KeyCode == Keys.Y || (e.Shift && e.KeyCode == Keys.Z)))
        {
            Redo();
            e.Handled = true;
            return;
        }

        if (e.Control && e.KeyCode == Keys.C && _state == State.Selected)
        {
            CopyToClipboard();
            e.Handled = true;
            return;
        }

        if (e.Control && e.KeyCode == Keys.S && _state == State.Selected)
        {
            SaveToFile();
            e.Handled = true;
            return;
        }

        if (_state == State.Selected && !e.Control && !e.Alt && !e.Shift)
        {
            Tool? newTool = e.KeyCode switch
            {
                Keys.P => Tool.Pen,
                Keys.L => Tool.Line,
                Keys.A => Tool.Arrow,
                Keys.R => Tool.Rectangle,
                Keys.E => Tool.Ellipse,
                Keys.M => Tool.Marker,
                Keys.T => Tool.Text,
                Keys.N => Tool.Step,
                Keys.B => Tool.Pixelate,
                _ => null,
            };
            if (newTool.HasValue)
            {
                SelectTool(newTool.Value);
                e.Handled = true;
                return;
            }
        }

        base.OnKeyDown(e);
    }

    // ═══════════════════════════════════════════
    //  TOOLBARS
    // ═══════════════════════════════════════════

    private void ShowToolbars()
    {
        HideToolbars();
        CreateToolPanel();
        CreateActionPanel();
        PositionToolbars();
    }

    private void HideToolbars()
    {
        if (_toolPanel != null) { Controls.Remove(_toolPanel); _toolPanel.Dispose(); _toolPanel = null; }
        if (_actionPanel != null) { Controls.Remove(_actionPanel); _actionPanel.Dispose(); _actionPanel = null; }
        _toolButtons.Clear();
        _colorBtn = null;
    }

    private void PositionToolbars()
    {
        if (_toolPanel == null || _actionPanel == null) return;

        int tx = _selection.Right + 5;
        int ty = _selection.Top;
        if (tx + _toolPanel.Width > ClientSize.Width)
            tx = _selection.Left - _toolPanel.Width - 5;
        if (ty + _toolPanel.Height > ClientSize.Height)
            ty = ClientSize.Height - _toolPanel.Height;
        _toolPanel.Location = new Point(Math.Max(0, tx), Math.Max(0, ty));

        int ax = _selection.Right - _actionPanel.Width;
        int ay = _selection.Bottom + 5;
        if (ay + _actionPanel.Height > ClientSize.Height)
            ay = _selection.Top - _actionPanel.Height - 5;
        if (ax < 0) ax = _selection.Left;
        _actionPanel.Location = new Point(Math.Max(0, ax), Math.Max(0, ay));
    }

    private void CreateToolPanel()
    {
        _toolPanel = new Panel { BackColor = Color.FromArgb(45, 45, 45), AutoSize = true, Padding = new Padding(3) };
        var flow = new FlowLayoutPanel
        {
            FlowDirection = FlowDirection.TopDown,
            AutoSize = true,
            WrapContents = false,
            BackColor = Color.Transparent,
            Padding = new Padding(1),
        };

        (string tip, Tool tool, Action<Graphics, Rectangle> icon)[] tools =
        {
            ("Pen (P)", Tool.Pen, IconPen),
            ("Line (L)", Tool.Line, IconLine),
            ("Arrow (A)", Tool.Arrow, IconArrow),
            ("Rectangle (R)", Tool.Rectangle, IconRect),
            ("Ellipse (E)", Tool.Ellipse, IconEllipse),
            ("Marker (M)", Tool.Marker, IconMarker),
            ("Text (T)", Tool.Text, IconText),
            ("Numbered step (N)", Tool.Step, IconStep),
            ("Pixelate / blur (B)", Tool.Pixelate, IconPixelate),
        };

        foreach (var (tip, tool, icon) in tools)
        {
            var btn = MakeButton(tip, icon, () => SelectTool(tool));
            btn.Tag = tool;
            if (tool == _tool) btn.BackColor = Color.FromArgb(80, 80, 80);
            _toolButtons.Add(btn);
            flow.Controls.Add(btn);
        }

        flow.Controls.Add(new Panel
        {
            Width = 28,
            Height = 2,
            BackColor = Color.FromArgb(70, 70, 70),
            Margin = new Padding(1, 4, 1, 4)
        });

        _colorBtn = MakeButton("Color", (g, r) =>
        {
            using var b = new SolidBrush(_color);
            g.FillEllipse(b, r.X + 3, r.Y + 3, r.Width - 6, r.Height - 6);
        }, ShowColorPicker);
        flow.Controls.Add(_colorBtn);

        flow.Controls.Add(MakeButton("Thickness", (g, r) =>
        {
            using var p = new Pen(Color.White, Math.Min(_thickness, 6))
            { StartCap = LineCap.Round, EndCap = LineCap.Round };
            g.DrawLine(p, r.Left + 3, r.Top + r.Height / 2, r.Right - 3, r.Top + r.Height / 2);
        }, CycleThickness));

        flow.Controls.Add(MakeButton("Undo (Ctrl+Z)", IconUndo, Undo));
        flow.Controls.Add(MakeButton("Redo (Ctrl+Y)", IconRedo, Redo));

        _toolPanel.Controls.Add(flow);
        Controls.Add(_toolPanel);
        _toolPanel.BringToFront();
    }

    private void CreateActionPanel()
    {
        _actionPanel = new Panel { BackColor = Color.FromArgb(45, 45, 45), AutoSize = true, Padding = new Padding(3) };
        var flow = new FlowLayoutPanel
        {
            FlowDirection = FlowDirection.LeftToRight,
            AutoSize = true,
            WrapContents = false,
            BackColor = Color.Transparent,
            Padding = new Padding(1),
        };

        flow.Controls.Add(MakeButton("Pin to screen", IconPin, PinToScreen));
        flow.Controls.Add(MakeButton("Copy (Ctrl+C)", IconCopy, CopyToClipboard));
        flow.Controls.Add(MakeButton("Save (Ctrl+S)", IconSave, SaveToFile));
        flow.Controls.Add(MakeButton("Close (Esc)", IconClose, Close));

        _actionPanel.Controls.Add(flow);
        Controls.Add(_actionPanel);
        _actionPanel.BringToFront();
    }

    private static Button MakeButton(string tooltip, Action<Graphics, Rectangle> drawIcon, Action onClick)
    {
        var btn = new Button
        {
            Size = new Size(30, 30),
            FlatStyle = FlatStyle.Flat,
            BackColor = Color.FromArgb(55, 55, 55),
            Margin = new Padding(1),
            Cursor = Cursors.Hand,
            TabStop = false,
        };
        btn.FlatAppearance.BorderSize = 0;
        btn.FlatAppearance.MouseOverBackColor = Color.FromArgb(75, 75, 75);
        btn.FlatAppearance.MouseDownBackColor = Color.FromArgb(90, 90, 90);

        var img = new Bitmap(22, 22);
        using (var g = Graphics.FromImage(img))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            drawIcon(g, new Rectangle(2, 2, 18, 18));
        }
        btn.Image = img;
        // Button.Dispose doesn't dispose its `Image`, so wire it up explicitly.
        // Toolbars are torn down + rebuilt on every selection change /
        // CycleThickness — without this we leak a Bitmap per icon every cycle.
        btn.Disposed += (_, _) => btn.Image?.Dispose();

        var tt = new ToolTip();
        tt.SetToolTip(btn, tooltip);

        btn.Click += (_, _) => onClick();
        return btn;
    }

    private void SelectTool(Tool tool)
    {
        _tool = tool;
        foreach (var b in _toolButtons)
            b.BackColor = (b.Tag is Tool t && t == _tool)
                ? Color.FromArgb(80, 80, 80)
                : Color.FromArgb(55, 55, 55);
    }

    // ═══════════════════════════════════════════
    //  COLOR / THICKNESS
    // ═══════════════════════════════════════════

    private void ShowColorPicker()
    {
        DismissColorPopup();

        var palette = Palettes[_paletteIndex];

        _colorPopup = new Panel
        {
            Size = new Size(216, 296),
            BackColor = Color.FromArgb(45, 45, 45),
            BorderStyle = BorderStyle.FixedSingle,
        };

        var prevBtn = new Button
        {
            Location = new Point(6, 6),
            Size = new Size(32, 32),
            Text = "‹",
            FlatStyle = FlatStyle.Flat,
            ForeColor = Color.White,
            BackColor = Color.FromArgb(70, 70, 70),
            Cursor = Cursors.Hand,
            TabStop = false,
            Font = new Font("Segoe UI", 12f, FontStyle.Bold),
        };
        prevBtn.FlatAppearance.BorderSize = 0;
        prevBtn.FlatAppearance.MouseOverBackColor = Color.FromArgb(90, 90, 90);
        prevBtn.Click += (_, _) =>
        {
            _paletteIndex = (_paletteIndex + Palettes.Length - 1) % Palettes.Length;
            ShowColorPicker();
        };
        _colorPopup.Controls.Add(prevBtn);

        var paletteLabel = new Label
        {
            Location = new Point(42, 6),
            Size = new Size(132, 32),
            Text = palette.Name,
            ForeColor = Color.White,
            BackColor = Color.FromArgb(70, 70, 70),
            TextAlign = ContentAlignment.MiddleCenter,
            Font = new Font("Segoe UI", 10f, FontStyle.Bold),
            AutoEllipsis = true,
        };
        _colorPopup.Controls.Add(paletteLabel);

        var nextBtn = new Button
        {
            Location = new Point(178, 6),
            Size = new Size(32, 32),
            Text = "›",
            FlatStyle = FlatStyle.Flat,
            ForeColor = Color.White,
            BackColor = Color.FromArgb(70, 70, 70),
            Cursor = Cursors.Hand,
            TabStop = false,
            Font = new Font("Segoe UI", 12f, FontStyle.Bold),
        };
        nextBtn.FlatAppearance.BorderSize = 0;
        nextBtn.FlatAppearance.MouseOverBackColor = Color.FromArgb(90, 90, 90);
        nextBtn.Click += (_, _) =>
        {
            _paletteIndex = (_paletteIndex + 1) % Palettes.Length;
            ShowColorPicker();
        };
        _colorPopup.Controls.Add(nextBtn);

        var swatchPanel = new Panel
        {
            Location = new Point(8, 46),
            Size = new Size(200, 200),
            BackColor = Color.Transparent,
        };
        for (int i = 0; i < palette.Colors.Length; i++)
        {
            var c = palette.Colors[i];
            int row = i / 4, col = i % 4;
            var swatch = new Button
            {
                Size = new Size(44, 44),
                Location = new Point(col * 48, row * 48),
                FlatStyle = FlatStyle.Flat,
                BackColor = Color.FromArgb(255, c.R, c.G, c.B),
                Cursor = Cursors.Hand,
                TabStop = false,
            };
            bool isSelected = c.R == _color.R && c.G == _color.G && c.B == _color.B;
            swatch.FlatAppearance.BorderColor = isSelected ? Color.White : Color.FromArgb(80, 80, 80);
            swatch.FlatAppearance.BorderSize = isSelected ? 2 : 1;
            var captured = c;
            swatch.Click += (_, _) =>
            {
                _color = captured;
                DismissColorPopup();
                RefreshColorButton();
            };
            swatchPanel.Controls.Add(swatch);
        }
        _colorPopup.Controls.Add(swatchPanel);

        var custom = new Button
        {
            Text = "Custom color…",
            Location = new Point(6, 254),
            Size = new Size(204, 32),
            FlatStyle = FlatStyle.Flat,
            ForeColor = Color.White,
            BackColor = Color.FromArgb(70, 70, 70),
            Cursor = Cursors.Hand,
            TabStop = false,
            TextAlign = ContentAlignment.MiddleCenter,
            Font = new Font("Segoe UI", 9.5f),
        };
        custom.FlatAppearance.BorderSize = 0;
        custom.FlatAppearance.MouseOverBackColor = Color.FromArgb(90, 90, 90);
        custom.Click += (_, _) =>
        {
            using var dlg = new ColorDialog { Color = _color, FullOpen = true };
            if (dlg.ShowDialog() == DialogResult.OK) _color = dlg.Color;
            DismissColorPopup();
            RefreshColorButton();
        };
        _colorPopup.Controls.Add(custom);

        if (_toolPanel != null)
        {
            int px = _toolPanel.Left - _colorPopup.Width - 5;
            if (px < 0) px = _toolPanel.Right + 5;
            _colorPopup.Location = new Point(px, _toolPanel.Top);
        }

        Controls.Add(_colorPopup);
        _colorPopup.BringToFront();
    }

    private void DismissColorPopup()
    {
        if (_colorPopup == null) return;
        Controls.Remove(_colorPopup);
        _colorPopup.Dispose();
        _colorPopup = null;
    }

    private void RefreshColorButton()
    {
        if (_colorBtn == null) return;
        var img = new Bitmap(22, 22);
        using (var g = Graphics.FromImage(img))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            using var b = new SolidBrush(_color);
            g.FillEllipse(b, 5, 5, 12, 12);
        }
        _colorBtn.Image?.Dispose();
        _colorBtn.Image = img;
    }

    private void CycleThickness()
    {
        float[] sizes = { 1, 2, 3, 5, 8 };
        int idx = Array.IndexOf(sizes, _thickness);
        _thickness = sizes[(idx + 1) % sizes.Length];
        ShowToolbars();
    }

    // ═══════════════════════════════════════════
    //  ICON DRAWING
    // ═══════════════════════════════════════════

    private static void IconPen(Graphics g, Rectangle r)
    {
        // Recognizable pen: an angled body with a pointed nib and a short
        // ink trail behind it — reads as "pen" at 18 px in a way the old
        // bezier curve never did.
        using var body  = new Pen(Color.White, 3) { StartCap = LineCap.Round, EndCap = LineCap.Round };
        using var trail = new Pen(Color.FromArgb(180, 255, 255, 255), 1.5f)
            { StartCap = LineCap.Round, EndCap = LineCap.Round };

        // Pen body — diagonal from lower-left to upper-right
        g.DrawLine(body, r.Left + 5, r.Bottom - 4, r.Right - 4, r.Top + 4);
        // Nib (a small filled triangle at the lower-left tip)
        var nib = new[]
        {
            new Point(r.Left + 2, r.Bottom - 2),
            new Point(r.Left + 6, r.Bottom - 5),
            new Point(r.Left + 4, r.Bottom - 7),
        };
        g.FillPolygon(Brushes.White, nib);
        // Ink trail under the nib
        g.DrawLine(trail, r.Left + 1, r.Bottom - 1, r.Left + 7, r.Bottom - 1);
    }

    private static void IconLine(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 2) { StartCap = LineCap.Round, EndCap = LineCap.Round };
        g.DrawLine(p, r.Left + 2, r.Bottom - 2, r.Right - 2, r.Top + 2);
    }

    private static void IconArrow(Graphics g, Rectangle r)
    {
        // Pen doesn't dispose its CustomEndCap on its own — keep the cap in its
        // own `using` so each toolbar redraw doesn't leak a GDI handle.
        using var cap = new AdjustableArrowCap(4, 4);
        using var p   = new Pen(Color.White, 2) { StartCap = LineCap.Round, CustomEndCap = cap };
        g.DrawLine(p, r.Left + 2, r.Bottom - 2, r.Right - 2, r.Top + 2);
    }

    private static void IconRect(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 2);
        g.DrawRectangle(p, r.X + 2, r.Y + 4, r.Width - 4, r.Height - 8);
    }

    private static void IconEllipse(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 2);
        g.DrawEllipse(p, r.X + 2, r.Y + 4, r.Width - 4, r.Height - 8);
    }

    private static void IconMarker(Graphics g, Rectangle r)
    {
        // Highlighter shape: angled chisel-tip body laying on a baseline,
        // stroke of yellow under the body to read as "highlight".
        int midY = r.Top + r.Height / 2;
        using var stroke = new SolidBrush(Color.FromArgb(160, 255, 235, 0));
        g.FillRectangle(stroke, r.Left + 2, midY + 1, r.Width - 4, 4);

        // Marker body (angled rectangle) over the right end of the stroke
        var body = new[]
        {
            new Point(r.Right - 11, midY + 4),
            new Point(r.Right - 4,  midY - 3),
            new Point(r.Right - 1,  midY),
            new Point(r.Right - 8,  midY + 7),
        };
        using var bodyBrush = new SolidBrush(Color.FromArgb(255, 220, 220, 220));
        g.FillPolygon(bodyBrush, body);
        using var outline = new Pen(Color.White, 1);
        g.DrawPolygon(outline, body);
    }

    private static void IconText(Graphics g, Rectangle r)
    {
        using var f = new Font("Segoe UI", 12f, FontStyle.Bold);
        g.DrawString("A", f, Brushes.White, r.Left + 1, r.Top - 1);
    }

    private static void IconStep(Graphics g, Rectangle r)
    {
        using var fill = new SolidBrush(Color.FromArgb(255, 80, 80));
        using var border = new Pen(Color.White, 1.5f);
        g.FillEllipse(fill, r.X + 3, r.Y + 3, r.Width - 6, r.Height - 6);
        g.DrawEllipse(border, r.X + 3, r.Y + 3, r.Width - 6, r.Height - 6);
        using var f = new Font("Segoe UI", 8f, FontStyle.Bold);
        using var sf = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center };
        g.DrawString("1", f, Brushes.White, r, sf);
    }

    private static void IconPixelate(Graphics g, Rectangle r)
    {
        // 3x3 mosaic of varied shades reads as "pixelate / blur" much more
        // clearly than the old 4x4 strict checkerboard. The varied tones
        // imply image content being redacted, not just a chessboard.
        var shades = new byte[] { 230, 90, 170, 110, 200, 60, 80, 220, 140 };
        int s = (r.Width - 4) / 3;
        for (int i = 0; i < 9; i++)
        {
            int x = i % 3, y = i / 3;
            var v = shades[i];
            using var b = new SolidBrush(Color.FromArgb(v, v, v));
            g.FillRectangle(b, r.X + 2 + x * s, r.Y + 2 + y * s, s, s);
        }
        // Subtle border so it stays a single visual block at small sizes
        using var border = new Pen(Color.FromArgb(120, 255, 255, 255), 1);
        g.DrawRectangle(border, r.X + 2, r.Y + 2, s * 3, s * 3);
    }

    private static void IconUndo(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 2);
        g.DrawArc(p, r.X + 2, r.Y + 4, r.Width - 4, r.Height - 6, 180, 230);
        g.DrawLine(p, r.Left + 4, r.Top + 4, r.Left + 2, r.Top + 8);
        g.DrawLine(p, r.Left + 4, r.Top + 4, r.Left + 8, r.Top + 5);
    }

    private static void IconRedo(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 2);
        g.DrawArc(p, r.X + 2, r.Y + 4, r.Width - 4, r.Height - 6, 310, 230);
        g.DrawLine(p, r.Right - 4, r.Top + 4, r.Right - 2, r.Top + 8);
        g.DrawLine(p, r.Right - 4, r.Top + 4, r.Right - 8, r.Top + 5);
    }

    private static void IconPin(Graphics g, Rectangle r)
    {
        // Classic thumbtack: round head with a small gloss highlight, a
        // tapered metal stem, and a sharp point at the bottom. Reads as
        // "pin to screen" in any toolbar context.
        int cx = r.Left + r.Width / 2;
        int headTop = r.Top + 1;
        int headBot = r.Top + 10;

        using var headFill = new SolidBrush(Color.White);
        g.FillEllipse(headFill, cx - 5, headTop, 10, 9);

        using var headEdge = new Pen(Color.FromArgb(140, 100, 100, 110), 1);
        g.DrawEllipse(headEdge, cx - 5, headTop, 10, 9);

        // Gloss highlight on the head
        using var gloss = new SolidBrush(Color.FromArgb(180, 180, 180, 180));
        g.FillEllipse(gloss, cx - 3, headTop + 2, 4, 2);

        // Tapered stem (triangle from head to point)
        var stem = new[]
        {
            new Point(cx - 2, headBot - 1),
            new Point(cx + 2, headBot - 1),
            new Point(cx,     r.Bottom - 1),
        };
        using var stemFill = new SolidBrush(Color.FromArgb(255, 220, 220, 220));
        g.FillPolygon(stemFill, stem);
    }

    private static void IconCopy(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 1.5f);
        g.DrawRectangle(p, r.X + 2, r.Y + 1, 10, 12);
        g.DrawRectangle(p, r.X + 6, r.Y + 5, 10, 12);
    }

    private static void IconSave(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.White, 1.5f);
        g.DrawRectangle(p, r.X + 2, r.Y + 2, r.Width - 4, r.Height - 4);
        g.DrawRectangle(p, r.X + 5, r.Y + 2, 8, 6);
        g.FillRectangle(Brushes.White, r.X + 4, r.Bottom - 7, r.Width - 8, 5);
    }

    private static void IconClose(Graphics g, Rectangle r)
    {
        using var p = new Pen(Color.FromArgb(255, 100, 100), 2);
        g.DrawLine(p, r.Left + 4, r.Top + 4, r.Right - 4, r.Bottom - 4);
        g.DrawLine(p, r.Right - 4, r.Top + 4, r.Left + 4, r.Bottom - 4);
    }

    // ═══════════════════════════════════════════
    //  ACTIONS
    // ═══════════════════════════════════════════

    private Bitmap GetFinalImage()
    {
        var bmp = new Bitmap(_selection.Width, _selection.Height, PixelFormat.Format32bppArgb);
        using var g = Graphics.FromImage(bmp);
        g.SmoothingMode = SmoothingMode.AntiAlias;

        g.DrawImage(_screenshot,
            new Rectangle(0, 0, _selection.Width, _selection.Height),
            _selection, GraphicsUnit.Pixel);

        g.TranslateTransform(-_selection.X, -_selection.Y);
        foreach (var a in _annotations) a.Draw(g);
        g.ResetTransform();

        if (_settings.WatermarkEnabled
            && !string.IsNullOrWhiteSpace(_settings.WatermarkText)
            && bmp.Width >= 80 && bmp.Height >= 24)
        {
            DrawWatermark(g, _settings.WatermarkText, bmp.Width, bmp.Height);
        }
        return bmp;
    }

    private static void DrawWatermark(Graphics g, string text, int w, int h)
    {
        g.TextRenderingHint = System.Drawing.Text.TextRenderingHint.AntiAliasGridFit;
        using var f = new Font("Segoe UI", 9f, FontStyle.Italic | FontStyle.Bold);
        var sz = g.MeasureString(text, f);
        float x = w - sz.Width - 8;
        float y = h - sz.Height - 6;
        using var shadow = new SolidBrush(Color.FromArgb(160, 0, 0, 0));
        using var fill = new SolidBrush(Color.FromArgb(225, 255, 255, 255));
        g.DrawString(text, f, shadow, x + 1, y + 1);
        g.DrawString(text, f, fill, x, y);
    }

    private void CopyToClipboard()
    {
        // Clipboard.SetImage can throw under contention (another app holds
        // the clipboard, RDP-without-clipboard-redirection, etc.). Don't crash
        // the overlay — surface a message and stay open so the user can retry.
        try
        {
            using var img = GetFinalImage();
            Clipboard.SetImage(img);
            CaptureCompleted?.Invoke(this, "Copied to clipboard!");
            Close();
        }
        catch (Exception ex)
        {
            MessageBox.Show(this,
                $"Couldn't copy to the clipboard.\n\n{ex.Message}",
                "Kashot", MessageBoxButtons.OK, MessageBoxIcon.Warning);
        }
    }

    private void SaveToFile()
    {
        var initialDir = !string.IsNullOrWhiteSpace(_settings.SaveDirectory) && Directory.Exists(_settings.SaveDirectory)
            ? _settings.SaveDirectory
            : Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);

        using var dlg = new SaveFileDialog
        {
            Filter           = "PNG Image|*.png|JPEG Image|*.jpg|Bitmap|*.bmp",
            DefaultExt       = "png",
            FileName         = $"kashot_{DateTime.Now:yyyyMMdd_HHmmss}",
            InitialDirectory = initialDir,
        };

        if (dlg.ShowDialog() != DialogResult.OK) return;

        // Save can fail (disk full, permission denied, path too long, network
        // path unavailable). Surface the error and stay open instead of
        // tearing the overlay down on the user.
        try
        {
            using var img = GetFinalImage();
            var fmt = Path.GetExtension(dlg.FileName).ToLowerInvariant() switch
            {
                ".jpg" or ".jpeg" => ImageFormat.Jpeg,
                ".bmp"            => ImageFormat.Bmp,
                _                 => ImageFormat.Png,
            };
            img.Save(dlg.FileName, fmt);
            _settings.SaveDirectory = Path.GetDirectoryName(dlg.FileName) ?? _settings.SaveDirectory;
            CaptureCompleted?.Invoke(this, $"Saved to {dlg.FileName}");
            Close();
        }
        catch (Exception ex)
        {
            MessageBox.Show(this,
                $"Couldn't save the image.\n\n{ex.Message}",
                "Kashot", MessageBoxButtons.OK, MessageBoxIcon.Warning);
        }
    }

    private void PinToScreen()
    {
        var img = GetFinalImage();
        var screenLoc = PointToScreen(_selection.Location);
        var pin = new PinForm(img, screenLoc);
        pin.Show();
        CaptureCompleted?.Invoke(this, "Pinned to screen");
        Close();
    }

    // ═══════════════════════════════════════════
    //  CLEANUP
    // ═══════════════════════════════════════════

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        _settings.LastTool = _tool.ToString();
        _settings.LastColorArgb = _color.ToArgb();
        _settings.LastThickness = _thickness;
        _settings.PaletteIndex = _paletteIndex;
        _settings.Save();

        _screenshot?.Dispose();
        base.OnFormClosed(e);
    }

    protected override CreateParams CreateParams
    {
        get
        {
            var cp = base.CreateParams;
            cp.ExStyle |= 0x02000000;
            return cp;
        }
    }
}
