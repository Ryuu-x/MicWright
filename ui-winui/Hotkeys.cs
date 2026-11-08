using System.Runtime.InteropServices;
using Windows.System;

namespace MicWright;

/// <summary>A key plus modifiers, kept in app.json as one number (modifiers &lt;&lt; 16 | key).</summary>
public readonly record struct Hotkey(uint Mods, uint Key)
{
    public const uint Alt = 1, Ctrl = 2, Shift = 4;

    public uint Packed => Mods << 16 | Key;
    public static Hotkey FromPacked(uint packed) => new(packed >> 16, packed & 0xFFFF);

    public string Label
    {
        get
        {
            var parts = new List<string>();
            if ((Mods & Ctrl) != 0) parts.Add("Ctrl");
            if ((Mods & Alt) != 0) parts.Add("Alt");
            if ((Mods & Shift) != 0) parts.Add("Shift");
            parts.Add(KeyName((VirtualKey)Key));
            return string.Join("+", parts);
        }
    }

    private static string KeyName(VirtualKey key) => key switch
    {
        >= VirtualKey.Number0 and <= VirtualKey.Number9 => ((char)key).ToString(),
        >= VirtualKey.NumberPad0 and <= VirtualKey.NumberPad9 => $"Num {key - VirtualKey.NumberPad0}",
        _ => Punctuation.GetValueOrDefault((uint)key) ?? key.ToString(),
    };

    /// <summary>US layout names of the keys VirtualKey has no name for.</summary>
    private static readonly Dictionary<uint, string> Punctuation = new()
    {
        [186] = ";", [187] = "=", [188] = ",", [189] = "-", [190] = ".", [191] = "/",
        [192] = "`", [219] = "[", [220] = "\\", [221] = "]", [222] = "'",
    };

    public static bool IsModifier(VirtualKey key) => key is VirtualKey.Control or VirtualKey.LeftControl or VirtualKey.RightControl
        or VirtualKey.Shift or VirtualKey.LeftShift or VirtualKey.RightShift
        or VirtualKey.Menu or VirtualKey.LeftMenu or VirtualKey.RightMenu
        or VirtualKey.LeftWindows or VirtualKey.RightWindows;

    /// <summary>Shortcuts Windows and nearly every app rely on: never taken, or the PC stops making sense.</summary>
    private static readonly HashSet<Hotkey> Reserved =
    [
        new(Alt, (uint)VirtualKey.F4), new(Alt, (uint)VirtualKey.Tab), new(Alt | Shift, (uint)VirtualKey.Tab),
        new(Alt, (uint)VirtualKey.Escape), new(Alt, (uint)VirtualKey.Space), new(Ctrl, (uint)VirtualKey.Escape),
        new(Ctrl | Shift, (uint)VirtualKey.Escape), new(Ctrl | Alt, (uint)VirtualKey.Delete),
        new(0, (uint)VirtualKey.Snapshot), new(0, (uint)VirtualKey.F12), // Windows keeps F12 for debuggers
        .. "ACVXYZS".Select(letter => new Hotkey(Ctrl, letter)),
    ];

    /// <summary>Why this combination can't be used, or null if it can.</summary>
    public string? Problem()
    {
        var key = (VirtualKey)Key;
        if (Mods == 0 && key is VirtualKey.Enter or VirtualKey.Space or VirtualKey.Back or VirtualKey.Tab or VirtualKey.Escape)
            return $"{Label} is needed everywhere, so it can't be a hotkey.";
        return Reserved.Contains(this) ? $"{Label} is a system shortcut, so it can't be a hotkey." : null;
    }

    /// <summary>
    /// A hotkey takes its key away from every other app, so a plain key can no longer be
    /// typed. That's allowed, but the user should know before saving it.
    /// </summary>
    public string? Warning()
    {
        var key = (VirtualKey)Key;
        var functionKey = key >= VirtualKey.F1 && key <= VirtualKey.F24;
        return Mods is 0 or Shift && !functionKey
            ? $"While Hotkeys is on, {Label} plays the sound instead of doing its normal job in every app."
            : null;
    }
}

/// <summary>
/// System-wide hotkeys through RegisterHotKey on a hidden window, so they keep working
/// from the tray. Unlike a keyboard hook this sees only the combinations registered here,
/// not what the user types, and it needs no special rights.
/// </summary>
public sealed class Hotkeys : IDisposable
{
    private const string ClassName = "MicWrightHotkeys";
    private const uint WmHotkey = 0x0312;
    private const uint ModNoRepeat = 0x4000; // holding the key must not retrigger it
    private static readonly IntPtr MessageOnly = new(-3); // HWND_MESSAGE

    private readonly WndProc _wndProc; // kept alive: native code holds a pointer to it
    private readonly IntPtr _hwnd;
    private readonly HashSet<int> _registered = [];

    public event Action<int>? Pressed;

    public Hotkeys()
    {
        _wndProc = Proc;
        var hInstance = GetModuleHandleW(null);
        var wc = new WndClassEx
        {
            cbSize = (uint)Marshal.SizeOf<WndClassEx>(),
            lpfnWndProc = Marshal.GetFunctionPointerForDelegate(_wndProc),
            hInstance = hInstance,
            lpszClassName = ClassName,
        };
        RegisterClassExW(ref wc);
        _hwnd = CreateWindowExW(0, ClassName, ClassName, 0, 0, 0, 0, 0, MessageOnly, IntPtr.Zero, hInstance, IntPtr.Zero);
    }

    /// <summary>False when another app already owns the combination.</summary>
    public bool Register(int id, Hotkey key)
    {
        var ok = RegisterHotKey(_hwnd, id, key.Mods | ModNoRepeat, key.Key);
        if (ok)
            _registered.Add(id);
        return ok;
    }

    public void Clear()
    {
        foreach (var id in _registered)
            UnregisterHotKey(_hwnd, id);
        _registered.Clear();
    }

    private IntPtr Proc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WmHotkey)
        {
            Pressed?.Invoke((int)wParam);
            return IntPtr.Zero;
        }
        return DefWindowProcW(hwnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        Clear();
        DestroyWindow(_hwnd);
    }

    // ------------------------------------------------------------------ Win32

    private delegate IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WndClassEx
    {
        public uint cbSize, style;
        public IntPtr lpfnWndProc;
        public int cbClsExtra, cbWndExtra;
        public IntPtr hInstance, hIcon, hCursor, hbrBackground;
        public string? lpszMenuName, lpszClassName;
        public IntPtr hIconSm;
    }

    [DllImport("user32.dll", SetLastError = true)] private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint mods, uint key);
    [DllImport("user32.dll")] private static extern bool UnregisterHotKey(IntPtr hwnd, int id);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern ushort RegisterClassExW(ref WndClassEx wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(uint exStyle, string cls, string name, uint style, int x, int y, int w, int h,
                                                 IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr DefWindowProcW(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr GetModuleHandleW(string? name);
}
