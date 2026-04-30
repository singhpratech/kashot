namespace Kashot;

public sealed class ThemeColors
{
    public Color Background { get; init; }
    public Color Surface { get; init; }
    public Color SurfaceAlt { get; init; }
    public Color Border { get; init; }
    public Color Text { get; init; }
    public Color TextMuted { get; init; }
    public Color Accent { get; init; }
    public Color ButtonBg { get; init; }
    public Color ButtonHover { get; init; }

    public static ThemeColors Light { get; } = new()
    {
        Background = Color.FromArgb(245, 245, 247),
        Surface = Color.White,
        SurfaceAlt = Color.FromArgb(235, 235, 240),
        Border = Color.FromArgb(210, 210, 215),
        Text = Color.FromArgb(30, 30, 30),
        TextMuted = Color.FromArgb(110, 110, 120),
        Accent = Color.FromArgb(88, 86, 214),
        ButtonBg = Color.FromArgb(228, 228, 232),
        ButtonHover = Color.FromArgb(215, 215, 220),
    };

    public static ThemeColors Dark { get; } = new()
    {
        Background = Color.FromArgb(32, 32, 36),
        Surface = Color.FromArgb(45, 45, 50),
        SurfaceAlt = Color.FromArgb(60, 60, 66),
        Border = Color.FromArgb(70, 70, 76),
        Text = Color.FromArgb(235, 235, 238),
        TextMuted = Color.FromArgb(160, 160, 168),
        Accent = Color.FromArgb(120, 118, 240),
        ButtonBg = Color.FromArgb(70, 70, 76),
        ButtonHover = Color.FromArgb(90, 90, 96),
    };

    public static ThemeColors For(string name) =>
        string.Equals(name, "Dark", StringComparison.OrdinalIgnoreCase) ? Dark : Light;
}
