using System.Runtime.InteropServices;

namespace MicWright;

/// <summary>
/// Switches the Windows default recording device, so every app that uses "Default" mic
/// hears the enhanced voice (the virtual cable) instead of the raw QuadCast.
/// Uses the undocumented IPolicyConfig, as EarTrumpet and SoundSwitch do.
/// </summary>
public static class DefaultMic
{
    public sealed record Endpoint(string Id, string Name);

    /// <summary>Active recording devices.</summary>
    public static List<Endpoint> Captures()
    {
        var list = new List<Endpoint>();
        var en = (IMMDeviceEnumerator)new MMDeviceEnumerator();
        if (en.EnumAudioEndpoints(EDataFlow.Capture, DeviceStateActive, out var coll) != 0)
            return list;
        coll.GetCount(out var n);
        for (uint i = 0; i < n; i++)
        {
            if (coll.Item(i, out var dev) != 0)
                continue;
            dev.GetId(out var id);
            list.Add(new(id, FriendlyName(dev)));
        }
        return list;
    }

    /// <summary>Id of the current default mic for a role, or null.</summary>
    public static string? Current(ERole role)
    {
        var en = (IMMDeviceEnumerator)new MMDeviceEnumerator();
        if (en.GetDefaultAudioEndpoint(EDataFlow.Capture, role, out var dev) != 0)
            return null;
        dev.GetId(out var id);
        return id;
    }

    /// <summary>The recording side of the cable we send to: "CABLE Input (X)" → "CABLE Output (X)".</summary>
    public static Endpoint? CableFor(string? send)
    {
        var caps = Captures();
        if (send is not null)
        {
            var wanted = send.Replace("Input", "Output", StringComparison.OrdinalIgnoreCase);
            if (caps.FirstOrDefault(c => c.Name.Equals(wanted, StringComparison.OrdinalIgnoreCase)) is { } exact)
                return exact;
        }
        return caps.FirstOrDefault(c => c.Name.StartsWith("CABLE Output", StringComparison.OrdinalIgnoreCase));
    }

    /// <summary>
    /// Calls <paramref name="changed"/> when an audio device appears or goes away. It runs on a
    /// Windows audio thread, so it must return quickly and not touch the UI.
    /// </summary>
    public static IDisposable WatchDevices(Action changed) => new DeviceWatcher(changed);

    /// <summary>Default mic for every role (console, multimedia, communications).</summary>
    public static bool SetAll(string id) =>
        Set(id, ERole.Console) & Set(id, ERole.Multimedia) & Set(id, ERole.Communications);

    public static bool Set(string id, ERole role)
    {
        try { return ((IPolicyConfig)new PolicyConfigClient()).SetDefaultEndpoint(id, role) == 0; }
        catch (COMException) { return false; }
    }

    public static bool Exists(string? id) => id is not null && Captures().Any(c => c.Id == id);

    /// <summary>
    /// Makes the cable the default mic, remembering the previous default in <paramref name="prefs"/>.
    /// Returns null on success, otherwise a message for the user.
    /// </summary>
    public static string? Apply(Prefs prefs, string? send)
    {
        if (CableFor(send) is not { } cable)
            return "No virtual cable found. Install VB-CABLE (vb-audio.com/Cable) and choose CABLE Input under Send to.";
        var console = Current(ERole.Console);
        if (console is not null && console != cable.Id)
        {
            prefs.PrevDefaultMic = console;
            prefs.PrevCommsMic = Current(ERole.Communications) is { } comms && comms != cable.Id ? comms : console;
            prefs.Save();
        }
        return SetAll(cable.Id) ? null : "Windows refused to change the default microphone.";
    }

    /// <summary>
    /// Puts the remembered default mic back (or the QuadCast), but only while the cable is still
    /// the default: if the user picked another mic in Windows since, that choice stays.
    /// </summary>
    public static void Restore(Prefs prefs)
    {
        var caps = Captures();
        var current = Current(ERole.Console);
        if (caps.FirstOrDefault(c => c.Id == current) is not { } now || !IsCable(now.Name))
            return;
        var fallback = caps.FirstOrDefault(c => c.Name.Contains("quadcast", StringComparison.OrdinalIgnoreCase))
                       ?? caps.FirstOrDefault(c => !IsCable(c.Name));
        var main = caps.Any(c => c.Id == prefs.PrevDefaultMic) ? prefs.PrevDefaultMic : fallback?.Id;
        var comms = caps.Any(c => c.Id == prefs.PrevCommsMic) ? prefs.PrevCommsMic : main;
        if (main is null)
            return;
        Set(main, ERole.Console);
        Set(main, ERole.Multimedia);
        Set(comms!, ERole.Communications);
    }

    public static bool IsCable(string name) =>
        name.Contains("cable", StringComparison.OrdinalIgnoreCase) || name.Contains("virtual", StringComparison.OrdinalIgnoreCase);

    private static string FriendlyName(IMMDevice dev)
    {
        if (dev.OpenPropertyStore(StgmRead, out var store) != 0)
            return "";
        var key = PkeyDeviceFriendlyName;
        if (store.GetValue(ref key, out var pv) != 0)
            return "";
        try { return pv.Vt == VtLpwstr ? Marshal.PtrToStringUni(pv.Ptr) ?? "" : ""; }
        finally { PropVariantClear(ref pv); }
    }

    // ------------------------------------------------------------------ COM interop

    public enum ERole { Console = 0, Multimedia = 1, Communications = 2 }
    private enum EDataFlow { Render = 0, Capture = 1 }

    private const uint DeviceStateActive = 1;
    private const int StgmRead = 0;
    private const ushort VtLpwstr = 31;
    private static readonly PropertyKey PkeyDeviceFriendlyName = new(new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"), 14);

    [StructLayout(LayoutKind.Sequential)]
    private struct PropertyKey(Guid fmtid, uint pid)
    {
        public Guid Fmtid = fmtid;
        public uint Pid = pid;
    }

    [StructLayout(LayoutKind.Explicit, Size = 24)]
    private struct PropVariant
    {
        [FieldOffset(0)] public ushort Vt;
        [FieldOffset(8)] public IntPtr Ptr;
    }

    [DllImport("ole32.dll")]
    private static extern int PropVariantClear(ref PropVariant pv);

    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    private class MMDeviceEnumerator;

    [ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
    private class PolicyConfigClient;

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(EDataFlow flow, uint stateMask, out IMMDeviceCollection devices);
        [PreserveSig] int GetDefaultAudioEndpoint(EDataFlow flow, ERole role, out IMMDevice device);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out IMMDevice device);
        [PreserveSig] int RegisterEndpointNotificationCallback(IMMNotificationClient client);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IMMNotificationClient client);
    }

    /// <summary>Public so Windows can call back into it. Runs on Windows' own audio threads.</summary>
    [ComImport, Guid("7991EEC9-7E89-4D85-8390-6C703CEC60C0"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMNotificationClient
    {
        [PreserveSig] int OnDeviceStateChanged([MarshalAs(UnmanagedType.LPWStr)] string id, uint state);
        [PreserveSig] int OnDeviceAdded([MarshalAs(UnmanagedType.LPWStr)] string id);
        [PreserveSig] int OnDeviceRemoved([MarshalAs(UnmanagedType.LPWStr)] string id);
        [PreserveSig] int OnDefaultDeviceChanged(int flow, int role, [MarshalAs(UnmanagedType.LPWStr)] string? id);
        [PreserveSig] int OnPropertyValueChanged([MarshalAs(UnmanagedType.LPWStr)] string id, IntPtr key);
    }

    public sealed class DeviceWatcher : IMMNotificationClient, IDisposable
    {
        private readonly IMMDeviceEnumerator _enumerator = (IMMDeviceEnumerator)new MMDeviceEnumerator();
        private readonly Action _changed;

        internal DeviceWatcher(Action changed)
        {
            _changed = changed;
            _enumerator.RegisterEndpointNotificationCallback(this);
        }

        public void Dispose() => _enumerator.UnregisterEndpointNotificationCallback(this);

        public int OnDeviceStateChanged(string id, uint state) => Notify();
        public int OnDeviceAdded(string id) => Notify();
        public int OnDeviceRemoved(string id) => Notify();
        // Ignored on purpose: we change the default device ourselves.
        public int OnDefaultDeviceChanged(int flow, int role, string? id) => 0;
        public int OnPropertyValueChanged(string id, IntPtr key) => 0;

        private int Notify()
        {
            _changed();
            return 0;
        }
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceCollection
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int Item(uint index, out IMMDevice device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, out IntPtr iface);
        [PreserveSig] int OpenPropertyStore(int access, out IPropertyStore store);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
    }

    [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int GetAt(uint index, out PropertyKey key);
        [PreserveSig] int GetValue(ref PropertyKey key, out PropVariant value);
    }

    /// <summary>Only SetDefaultEndpoint is used; the earlier slots just keep the vtable order.</summary>
    [ComImport, Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPolicyConfig
    {
        [PreserveSig] int GetMixFormat(IntPtr a, IntPtr b);
        [PreserveSig] int GetDeviceFormat(IntPtr a, int b, IntPtr c);
        [PreserveSig] int ResetDeviceFormat(IntPtr a);
        [PreserveSig] int SetDeviceFormat(IntPtr a, IntPtr b, IntPtr c);
        [PreserveSig] int GetProcessingPeriod(IntPtr a, int b, IntPtr c, IntPtr d);
        [PreserveSig] int SetProcessingPeriod(IntPtr a, IntPtr b);
        [PreserveSig] int GetShareMode(IntPtr a, IntPtr b);
        [PreserveSig] int SetShareMode(IntPtr a, IntPtr b);
        [PreserveSig] int GetPropertyValue(IntPtr a, IntPtr b, IntPtr c);
        [PreserveSig] int SetPropertyValue(IntPtr a, IntPtr b, IntPtr c);
        [PreserveSig] int SetDefaultEndpoint([MarshalAs(UnmanagedType.LPWStr)] string id, ERole role);
    }
}
