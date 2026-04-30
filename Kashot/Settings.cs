using System.Text.Json;
using System.Text.Json.Serialization;

namespace Kashot;

public sealed class AppSettings
{
    public string LastTool { get; set; } = "Pen";
    public int LastColorArgb { get; set; } = unchecked((int)0xFFFF0000);
    public float LastThickness { get; set; } = 3f;
    public string SaveDirectory { get; set; } = "";
    public uint HotkeyModifiers { get; set; } = 0;
    public uint HotkeyVirtualKey { get; set; } = 0x2C;
    public bool StartWithWindows { get; set; } = false;
    public bool WatermarkEnabled { get; set; } = true;
    public string WatermarkText { get; set; } = "PrateekSingh";
    public int PaletteIndex { get; set; } = 0;

    [JsonIgnore]
    public Color LastColor
    {
        get => Color.FromArgb(LastColorArgb);
        set => LastColorArgb = value.ToArgb();
    }

    public static string AppDataDir => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Kashot");

    private static string SettingsPath => Path.Combine(AppDataDir, "settings.json");

    public static AppSettings Load()
    {
        try
        {
            if (File.Exists(SettingsPath))
            {
                var json = File.ReadAllText(SettingsPath);
                return JsonSerializer.Deserialize<AppSettings>(json) ?? new AppSettings();
            }
        }
        catch { }
        return new AppSettings();
    }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(AppDataDir);
            var json = JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true });
            File.WriteAllText(SettingsPath, json);
        }
        catch { }
    }
}
