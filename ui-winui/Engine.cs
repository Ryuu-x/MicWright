using System.Runtime.InteropServices;
using System.Text.Json;

namespace MicWright;

/// <summary>Mirror of mw_dsp::Meters (#[repr(C)], 11 × f32 + u32).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct Meters
{
    public float InPeakDb, InRmsDb, OutPeakDb, OutRmsDb, Vad, NoiseFloorDb, AgcGainDb, ExpGainDb, CompGrDb, DeessGrDb, LimiterGrDb;
    public uint Blocks;
}

public sealed record KnobInfo(string Key, string Label, string Hint, float Min, float Max, float Step, string Format, bool OnlyWhenAgcOff, bool Simple);
public sealed record GroupInfo(string Title, string Hint, string? Toggle, List<KnobInfo> Knobs);
public sealed record PageInfo(string Name, string Blurb, List<GroupInfo> Groups);
public sealed record PresetInfo(string Key, string Name, string Tip);
public sealed record DeviceList(List<string> Inputs, List<string> Outputs, string? DefaultInput, string? DefaultSend, string? DefaultMonitor);
public sealed record EngineStatus(bool Running, float LatencyMs, string Input, string Output, uint Dropouts);
public sealed record TipInfo(string Key, string Severity, string Title, string Message);
public sealed record ClipInfo(string Id, string Name, float Seconds);
public sealed record PreparedClip(string? Id, string? Name, float Seconds, string? Error);
public sealed record PlayingClip(string Id, bool Preview, float SecondsLeft);

/// <summary>Thin managed wrapper over the Rust engine's C ABI (crates/mw-engine/src/ffi.rs).</summary>
public sealed partial class Engine : IDisposable
{
    private const string Dll = "mw_engine";
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private IntPtr _handle = mw_new();

    public static List<PageInfo> Settings() => Parse<List<PageInfo>>(mw_settings_json()) ?? [];
    public static List<PresetInfo> Presets() => Parse<List<PresetInfo>>(mw_presets_json()) ?? [];
    public static DeviceList Devices() => Parse<DeviceList>(mw_devices_json()) ?? new([], [], null, null, null);
    public static string RecordingsDir() => Take(mw_recordings_dir()) ?? "";

    public bool Start(string input, string send, string? monitor) => mw_start(_handle, input, send, monitor) == 1;
    public void Stop() => mw_stop(_handle);
    public string? LastError => Take(mw_last_error(_handle));
    public EngineStatus Status => Parse<EngineStatus>(mw_status_json(_handle)) ?? new(false, 0, "", "", 0);

    public bool Monitor
    {
        get => mw_get_monitor(_handle) != 0;
        set => mw_set_monitor(_handle, value ? 1 : 0);
    }

    public float Get(string key) => mw_param_get(_handle, key);
    public bool Set(string key, float value) => mw_param_set(_handle, key, value) == 1;
    public bool Enhanced => Get("enabled") >= 0.5f;
    public bool Muted => Get("mute") >= 0.5f;
    public void ToggleAb() => mw_toggle_ab(_handle);
    public void ToggleMute() => mw_toggle_mute(_handle);

    /// <summary>"far" / "quiet" / "noisy" / "beatbox" / "custom:&lt;name&gt;".</summary>
    public bool ApplyPreset(string key) => mw_preset_apply(_handle, key) == 1;
    /// <summary>Key of the applied preset, or "" once values were edited since.</summary>
    public string CurrentPreset => Take(mw_preset_current(_handle)) ?? "";
    /// <summary>Key of the preset picked last, still known after edits; "" when there is none.</summary>
    public string PickedPreset => Take(mw_preset_picked(_handle)) ?? "";
    /// <summary>Undoes every edit since the last preset was picked.</summary>
    public void RevertPreset() => mw_preset_revert(_handle);
    public void ResetPage(int page) => mw_reset_page(_handle, (uint)page);

    public List<string> CustomPresets() => Parse<List<string>>(mw_custom_presets_json(_handle)) ?? [];
    /// <summary>Returns null on success, otherwise a message for the user.</summary>
    public string? SavePreset(string name) => Take(mw_preset_save(_handle, name));
    public bool DeletePreset(string name) => mw_preset_delete(_handle, name) == 1;
    public void LoadState() => mw_load_state(_handle);
    public void SaveState() => mw_save_state(_handle);

    public (Meters meters, float peakHold) Poll()
    {
        mw_poll(_handle, out var m, out var hold);
        return (m, hold);
    }

    public List<TipInfo> Tips() => Parse<List<TipInfo>>(mw_tips_json(_handle)) ?? [];

    public void StartRecording(uint seconds) => mw_record_start(_handle, seconds);
    public void StopRecording() => mw_record_stop(_handle);
    /// <summary>Seconds left, or null when not recording.</summary>
    public float? RecordingLeft => mw_record_left(_handle) is var s && s >= 0 ? s : null;
    public string? LastRecordingDir => Take(mw_last_recording_dir(_handle));

    /// <summary>Decodes and saves a sound file. Slow: run it off the UI thread. Doesn't touch the engine.</summary>
    public static PreparedClip PrepareClip(string path) => Parse<PreparedClip>(mw_board_prepare(path)) ?? new(null, null, 0, "couldn't be read");
    public void LoadBoard() => mw_board_load(_handle);
    public List<ClipInfo> Clips() => Parse<List<ClipInfo>>(mw_board_clips_json(_handle)) ?? [];
    /// <summary>Returns null on success, otherwise a message for the user.</summary>
    public string? AddClip(string id, string name) => Take(mw_board_add(_handle, id, name));
    public bool RemoveClip(string id) => mw_board_remove(_handle, id) == 1;
    /// <summary>Returns null on success, otherwise a message for the user.</summary>
    public string? PlayClip(string id, bool preview) => Take(mw_board_play(_handle, id, preview ? 1 : 0));
    /// <summary>Stops the sound if it's playing, otherwise starts it. Null on success, otherwise a message.</summary>
    public string? ToggleClip(string id, bool preview) => Take(mw_board_toggle(_handle, id, preview ? 1 : 0));
    public List<PlayingClip> PlayingClips() => Parse<List<PlayingClip>>(mw_board_playing_json(_handle)) ?? [];
    public void StopClips() => mw_board_stop(_handle);
    public void SetBoardLevels(double send, double monitor) => mw_board_levels(_handle, (float)send, (float)monitor);

    public void Dispose()
    {
        if (_handle != IntPtr.Zero)
        {
            mw_free(_handle);
            _handle = IntPtr.Zero;
        }
    }

    private static string? Take(IntPtr p)
    {
        if (p == IntPtr.Zero) return null;
        try { return Marshal.PtrToStringUTF8(p); }
        finally { mw_free_string(p); }
    }

    private static T? Parse<T>(IntPtr p) => Take(p) is { } s ? JsonSerializer.Deserialize<T>(s, Json) : default;

    [LibraryImport(Dll)] private static partial void mw_free_string(IntPtr s);
    [LibraryImport(Dll)] private static partial IntPtr mw_settings_json();
    [LibraryImport(Dll)] private static partial IntPtr mw_presets_json();
    [LibraryImport(Dll)] private static partial IntPtr mw_devices_json();
    [LibraryImport(Dll)] private static partial IntPtr mw_recordings_dir();
    [LibraryImport(Dll)] private static partial IntPtr mw_new();
    [LibraryImport(Dll)] private static partial void mw_free(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial int mw_start(IntPtr c, string input, string send, string? monitor);
    [LibraryImport(Dll)] private static partial void mw_stop(IntPtr c);
    [LibraryImport(Dll)] private static partial IntPtr mw_last_error(IntPtr c);
    [LibraryImport(Dll)] private static partial IntPtr mw_status_json(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_set_monitor(IntPtr c, int on);
    [LibraryImport(Dll)] private static partial int mw_get_monitor(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial float mw_param_get(IntPtr c, string key);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial int mw_param_set(IntPtr c, string key, float value);
    [LibraryImport(Dll)] private static partial void mw_toggle_ab(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_toggle_mute(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial int mw_preset_apply(IntPtr c, string key);
    [LibraryImport(Dll)] private static partial IntPtr mw_preset_current(IntPtr c);
    [LibraryImport(Dll)] private static partial IntPtr mw_preset_picked(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_preset_revert(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_reset_page(IntPtr c, uint page);
    [LibraryImport(Dll)] private static partial IntPtr mw_custom_presets_json(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial IntPtr mw_preset_save(IntPtr c, string name);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial int mw_preset_delete(IntPtr c, string name);
    [LibraryImport(Dll)] private static partial void mw_load_state(IntPtr c);
    [LibraryImport(Dll)] private static partial int mw_save_state(IntPtr c);
    [LibraryImport(Dll)] private static partial int mw_poll(IntPtr c, out Meters meters, out float peakHoldDb);
    [LibraryImport(Dll)] private static partial IntPtr mw_tips_json(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_record_start(IntPtr c, uint seconds);
    [LibraryImport(Dll)] private static partial float mw_record_left(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_record_stop(IntPtr c);
    [LibraryImport(Dll)] private static partial IntPtr mw_last_recording_dir(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial IntPtr mw_board_prepare(string path);
    [LibraryImport(Dll)] private static partial void mw_board_load(IntPtr c);
    [LibraryImport(Dll)] private static partial IntPtr mw_board_clips_json(IntPtr c);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial IntPtr mw_board_add(IntPtr c, string id, string name);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial int mw_board_remove(IntPtr c, string id);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial IntPtr mw_board_play(IntPtr c, string id, int preview);
    [LibraryImport(Dll, StringMarshalling = StringMarshalling.Utf8)] private static partial IntPtr mw_board_toggle(IntPtr c, string id, int preview);
    [LibraryImport(Dll)] private static partial IntPtr mw_board_playing_json(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_board_stop(IntPtr c);
    [LibraryImport(Dll)] private static partial void mw_board_levels(IntPtr c, float send, float monitor);
}

public static class Format
{
    public static string Value(string format, float v) => format switch
    {
        "db" => MathF.Abs(v) < 0.05f ? "0 dB" : $"{v:+0.0;-0.0} dB",
        "hz" => $"{v:0} Hz",
        "percent" => v <= 0.001f ? "Off" : $"{v * 100:0}%",
        "ratio" => $"{v:0.0} : 1",
        "semitones" => MathF.Abs(v) < 0.05f ? "Natural" : $"{v:+0.0;-0.0} st",
        "cut" => v < 0.05f ? "Off" : Reduction(v),
        "scale" => $"{v * 100:0}%",
        _ => $"{v:0.##}",
    };

    /// <summary>How far something was turned down; the sign of <paramref name="db"/> doesn't matter.</summary>
    public static string Reduction(float db) => MathF.Abs(db) < 0.05f ? "0 dB" : $"-{MathF.Abs(db):0.0} dB";

    public static string Level(float db) => db < -90 ? "Silent" : $"{db:0.0} dBFS";
}
