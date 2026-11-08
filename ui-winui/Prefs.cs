using System.Text.Json;
using Microsoft.Win32;

namespace MicWright;

/// <summary>App behaviour (not sound) settings, in %APPDATA%\MicWright\app.json.</summary>
public sealed class Prefs
{
    private static readonly string FilePath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "MicWright", "app.json");
    private static readonly string SetupMarker = Path.Combine(Path.GetDirectoryName(FilePath)!, "setup-done");
    private static readonly JsonSerializerOptions Json = new() { WriteIndented = true };

    // The app used to be called QuadCast Enhancer: carry its settings and startup entry over once.
    private const string LegacyName = "QuadCastEnhancer";
    private const string LegacyRunValue = "QuadCast Enhancer";

    static Prefs()
    {
        try
        {
            var dir = Path.GetDirectoryName(FilePath)!;
            var legacy = Path.Combine(Path.GetDirectoryName(dir)!, LegacyName);
            if (!Directory.Exists(dir) && Directory.Exists(legacy))
                Directory.Move(legacy, dir);
        }
        catch { /* worst case: default settings and the walkthrough once more */ }
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey, writable: true);
            if (key?.GetValue(LegacyRunValue) is string)
            {
                key.DeleteValue(LegacyRunValue, false);
                key.SetValue(RunValue, $"\"{Environment.ProcessPath}\" --background");
            }
        }
        catch { /* the user can turn "Start with Windows" back on */ }
    }

    /// <summary>Live pop-up hints (level, clipping, noise). Audio errors always show.</summary>
    public bool ShowTips { get; set; } = true;
    /// <summary>Make the virtual cable the Windows default mic while the app runs.</summary>
    public bool DefaultMic { get; set; }
    /// <summary>The Windows default mic before we replaced it (console, communications), to put back.</summary>
    public string? PrevDefaultMic { get; set; }
    public string? PrevCommsMic { get; set; }
    /// <summary>Show every sound setting instead of the few that matter most.</summary>
    public bool ShowAllSettings { get; set; }
    /// <summary>Devices the user picked; null means use the best guess.</summary>
    public string? InputDevice { get; set; }
    public string? SendDevice { get; set; }
    public string? MonitorDevice { get; set; }
    /// <summary>Hear yourself while you talk.</summary>
    public bool MonitorOn { get; set; } = true;
    /// <summary>Soundboard slider positions (0..1): what others hear, what you hear.</summary>
    public double BoardVolume { get; set; } = 0.7;
    public double BoardMonitorVolume { get; set; } = 0.7;
    public bool BoardHotkeys { get; set; } = true;
    public bool BoardStopKeyOn { get; set; } = true;
    /// <summary>Hotkey per sound id, and the one that stops everything (see <see cref="Hotkey"/>).</summary>
    public Dictionary<string, uint> BoardKeys { get; set; } = [];
    public uint BoardStopKey { get; set; } = new Hotkey(Hotkey.Ctrl | Hotkey.Alt, (uint)Windows.System.VirtualKey.End).Packed;

    /// <summary>
    /// The first-run walkthrough was finished or skipped. Kept in its own file because
    /// older installed versions rewrite app.json and drop every field they don't know.
    /// </summary>
    public static bool SetupDone
    {
        get => File.Exists(SetupMarker);
        set
        {
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(SetupMarker)!);
                File.WriteAllText(SetupMarker, "");
            }
            catch { /* worst case the walkthrough shows once more */ }
        }
    }

    public static Prefs Load()
    {
        try { return JsonSerializer.Deserialize<Prefs>(File.ReadAllText(FilePath)) ?? new(); }
        catch { return new(); }
    }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
            File.WriteAllText(FilePath, JsonSerializer.Serialize(this, Json));
        }
        catch { /* losing a preference is not worth a crash */ }
    }

    // ------------------------------------------------------------------ start with Windows

    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValue = "MicWright";

    public static bool StartWithWindows
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey);
            return key?.GetValue(RunValue) is string;
        }
        set
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKey);
            if (value)
                key.SetValue(RunValue, $"\"{Environment.ProcessPath}\" --background");
            else
                key.DeleteValue(RunValue, false);
        }
    }
}
