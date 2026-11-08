using System.Runtime.InteropServices;

namespace MicWright;

/// <summary>
/// Notification-area icon (Shell_NotifyIcon) on a hidden Win32 window of its own, so the
/// app can keep feeding the virtual mic with its main window closed.
/// Left click opens the window; right click shows Open / Quit.
/// </summary>
public sealed class TrayIcon : IDisposable
{
    private const string ClassName = "MicWrightTray";
    private const uint WmTray = 0x8000 + 1; // WM_APP + 1
    private const uint CmdOpen = 1, CmdQuit = 2;

    private readonly WndProc _wndProc; // kept alive: native code holds a pointer to it
    private readonly IntPtr _hwnd;
    private readonly IntPtr _icon;
    private readonly uint _taskbarCreated = RegisterWindowMessageW("TaskbarCreated");
    private readonly string _tip;
    private bool _added;

    public event Action? Open;
    public event Action? Quit;

    public TrayIcon(string tip, string iconPath)
    {
        _tip = tip;
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
        _hwnd = CreateWindowExW(0, ClassName, ClassName, 0, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, hInstance, IntPtr.Zero);
        var size = GetSystemMetrics(49); // SM_CXSMICON
        _icon = LoadImageW(IntPtr.Zero, iconPath, 1 /* IMAGE_ICON */, size, size, 0x10 /* LR_LOADFROMFILE */);
    }

    public bool Visible
    {
        get => _added;
        set
        {
            if (value == _added)
                return;
            var data = Data();
            _added = value && Shell_NotifyIconW(0 /* NIM_ADD */, ref data);
            if (!value)
                Shell_NotifyIconW(2 /* NIM_DELETE */, ref data);
        }
    }

    private NotifyIconData Data() => new()
    {
        cbSize = (uint)Marshal.SizeOf<NotifyIconData>(),
        hWnd = _hwnd,
        uID = 1,
        uFlags = 0x1 | 0x2 | 0x4, // NIF_MESSAGE | NIF_ICON | NIF_TIP
        uCallbackMessage = WmTray,
        hIcon = _icon,
        szTip = _tip,
        szInfo = "",
        szInfoTitle = "",
    };

    private IntPtr Proc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WmTray)
        {
            var mouse = (uint)(lParam.ToInt64() & 0xFFFF);
            if (mouse == 0x202) // WM_LBUTTONUP
                Open?.Invoke();
            else if (mouse == 0x205) // WM_RBUTTONUP
                ShowMenu();
            return IntPtr.Zero;
        }
        if (msg == _taskbarCreated && _added)
        {
            // Explorer restarted: the icon is gone, add it again.
            _added = false;
            Visible = true;
        }
        return DefWindowProcW(hwnd, msg, wParam, lParam);
    }

    private void ShowMenu()
    {
        var menu = CreatePopupMenu();
        AppendMenuW(menu, 0, CmdOpen, "Open MicWright");
        AppendMenuW(menu, 0x800 /* MF_SEPARATOR */, 0, null);
        AppendMenuW(menu, 0, CmdQuit, "Quit");
        GetCursorPos(out var pt);
        SetForegroundWindow(_hwnd); // so the menu closes when clicking elsewhere
        var cmd = TrackPopupMenu(menu, 0x100 | 0x2 /* TPM_RETURNCMD | TPM_RIGHTBUTTON */, pt.X, pt.Y, 0, _hwnd, IntPtr.Zero);
        DestroyMenu(menu);
        if (cmd == CmdOpen)
            Open?.Invoke();
        else if (cmd == CmdQuit)
            Quit?.Invoke();
    }

    public void Dispose()
    {
        Visible = false;
        DestroyWindow(_hwnd);
        if (_icon != IntPtr.Zero)
            DestroyIcon(_icon);
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

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NotifyIconData
    {
        public uint cbSize;
        public IntPtr hWnd;
        public uint uID, uFlags, uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string szTip;
        public uint dwState, dwStateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string szInfo;
        public uint uVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string szInfoTitle;
        public uint dwInfoFlags;
        public Guid guidItem;
        public IntPtr hBalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point { public int X, Y; }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern ushort RegisterClassExW(ref WndClassEx wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateWindowExW(uint exStyle, string cls, string name, uint style, int x, int y, int w, int h,
                                                 IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr DefWindowProcW(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern uint RegisterWindowMessageW(string name);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr GetModuleHandleW(string? name);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadImageW(IntPtr instance, string name, uint type, int cx, int cy, uint load);
    [DllImport("user32.dll")] private static extern bool DestroyIcon(IntPtr icon);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] private static extern bool Shell_NotifyIconW(uint message, ref NotifyIconData data);
    [DllImport("user32.dll")] private static extern IntPtr CreatePopupMenu();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern bool AppendMenuW(IntPtr menu, uint flags, uint id, string? text);
    [DllImport("user32.dll")] private static extern bool DestroyMenu(IntPtr menu);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out Point pt);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")]
    private static extern uint TrackPopupMenu(IntPtr menu, uint flags, int x, int y, int reserved, IntPtr hwnd, IntPtr rect);
}
