using CommunityToolkit.WinUI.Controls;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage.Pickers;
using Windows.UI.Core;
using VirtualKey = Windows.System.VirtualKey;

namespace MicWright;

public sealed partial class MainWindow : Window
{
    private const string MonitorOff = "Off";
    private const uint RecordSeconds = 15;
    // Pill colours, see Pill* in App.xaml.
    private const string Good = "Good", Warn = "Warn", Bad = "Bad", Off = "Off";
    private const int WaveBarCount = 72;
    /// <summary>The mic's good range. The ends match the engine's "quiet" and "clipping" tips.</summary>
    private const float QuietDb = -35, HotDb = -3, ClipDb = -1;
    private const float SilentDb = -60;
    private const double WaveFloor = 3;
    private const int StopHotkeyId = 1, FirstClipHotkeyId = 2;
    private const string PlayGlyph = "\uE768", StopGlyph = "\uE73B";
    private static readonly TimeSpan StoppedCue = TimeSpan.FromSeconds(2);
    private const string SimpleBlurb = "The settings that make the most difference. Turn on Advanced settings for the rest.";

    private readonly Engine _engine = new();
    private readonly List<PageInfo> _pages = Engine.Settings();
    private readonly List<PresetInfo> _presets = Engine.Presets();
    private readonly DispatcherQueueTimer _timer;
    private readonly HashSet<string> _dismissed = [];
    /// <summary>Slider / value label / card per knob key on the visible page.</summary>
    private readonly Dictionary<string, (Slider slider, TextBlock value, KnobInfo knob)> _knobs = [];
    private readonly Dictionary<string, TextBlock> _stats = [];
    private readonly Prefs _prefs = Prefs.Load();
    private readonly TrayIcon _tray;
    private readonly MediaPlayer _player = new();
    /// <summary>Waits for a burst of device changes to settle before rescanning.</summary>
    private readonly DispatcherQueueTimer _deviceTimer;
    private readonly IDisposable _deviceWatch;
    /// <summary>"Active" / "Edited" pill per preset key.</summary>
    private readonly Dictionary<string, Border> _presetPills = [];
    private readonly List<Microsoft.UI.Xaml.Shapes.Rectangle> _waveBars = [];
    /// <summary>Recent mic levels (0..1), oldest first, one per wave bar.</summary>
    private readonly double[] _wave = new double[WaveBarCount];
    private readonly Hotkeys _hotkeys = new();
    /// <summary>Which sound each registered hotkey plays.</summary>
    private readonly Dictionary<int, string> _hotkeyClips = [];
    /// <summary>Hotkey pill per sound id.</summary>
    private readonly Dictionary<string, Border> _clipPills = [];
    private readonly Dictionary<string, ClipRowParts> _clipRows = [];
    private sealed record ClipRowParts(ClipInfo Clip, Button Play, FontIcon Icon, Border State, ProgressBar Bar);

    private int _page;
    private bool _syncing;
    private string _tipsShown = "";
    private int _slowTick;
    private string? _defaultMicError;
    private bool _quitting;
    private Meters _meters;
    private float _hold = -120;
    private string _status = "";
    private SetupDialog? _setup;
    /// <summary>What "Undo reset" puts back; cleared by the next change to the sound.</summary>
    private Dictionary<string, float>? _undo;
    private (string raw, string enhanced)? _latest;
    private Button? _playing;
    private string? _playError;
    private bool _wasRecording;
    /// <summary>The user pressed Stop, so a device change mustn't start the engine again.</summary>
    private bool _stoppedByUser;
    private List<ClipInfo> _clips = [];
    private string _boardStatus = "";
    private bool _pickingHotkey;
    private bool _dialogOpen;
    private int _playingCount;
    private DateTime _stoppedUntil;

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        // The app is always dark, even on a light Windows theme.
        AppWindow.TitleBar.ButtonForegroundColor = Microsoft.UI.Colors.White;
        AppWindow.TitleBar.ButtonInactiveForegroundColor = Microsoft.UI.Colors.Gray;
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        AppWindow.SetIcon(icon);

        if (App.HasArg("--make-default"))
            _prefs.DefaultMic = true;

        // Developer shortcut to one sound page, so show all of them.
        if (ArgPage() is { } argPage)
        {
            _page = argPage;
            _prefs.ShowAllSettings = true;
        }
        Nav.SelectedItem = ArgPage() is null ? DashboardItem : SoundItem;

        _engine.LoadState(); // your last settings, from %APPDATA%\MicWright\last.json
        _engine.LoadBoard();
        BuildWave();
        BuildPresetCards();
        BuildStats();
        LoadDevices(Engine.Devices());
        BuildSoundboard();
        foreach (var p in _pages)
            PageBar.Items.Add(new SelectorBarItem { Text = p.Name });
        PageBar.SelectedItem = PageBar.Items[_page];

        StartEngine();
        _prefs.Save();
        SyncAppSwitches();
        RefreshAb();
        _latest = LatestRecording();
        _player.MediaEnded += (_, _) => DispatcherQueue.TryEnqueue(() => ShowPlaying(null));
        _player.MediaFailed += (_, e) => DispatcherQueue.TryEnqueue(() => ShowPlaying(null, $"Couldn't play the recording: {e.ErrorMessage}"));

        _deviceTimer = DispatcherQueue.CreateTimer();
        _deviceTimer.Interval = TimeSpan.FromMilliseconds(800);
        _deviceTimer.IsRepeating = false;
        _deviceTimer.Tick += (_, _) => RefreshDevices();
        _deviceWatch = DefaultMic.WatchDevices(() => DispatcherQueue.TryEnqueue(() =>
        {
            _deviceTimer.Stop();
            _deviceTimer.Start();
        }));

        _tray = new TrayIcon("MicWright", icon);
        _tray.Open += ShowFromTray;
        _tray.Quit += Quit;
        _tray.Visible = true;

        _timer = DispatcherQueue.CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(33);
        _timer.Tick += (_, _) => Tick();
        _timer.Start();

        // Other apps are listening to us: closing the window only hides it to the tray.
        AppWindow.Closing += (_, e) =>
        {
            // Alt+F4 pressed to try it as a hotkey still reaches the window.
            if (_quitting)
                return;
            if (_pickingHotkey)
            {
                e.Cancel = true;
                return;
            }
            if (!_prefs.DefaultMic)
                return;
            e.Cancel = true;
            AppWindow.Hide();
        };
        Closed += (_, _) =>
        {
            _timer.Stop();
            _deviceTimer.Stop();
            _deviceWatch.Dispose();
            _hotkeys.Dispose();
            _player.Dispose();
            _engine.SaveState();
            _engine.Dispose();
            // Without us the cable is silent, so hand the apps their raw mic back.
            if (_prefs.DefaultMic)
                DefaultMic.Restore(_prefs);
            _tray.Dispose();
        };
    }

    public void ShowFromTray()
    {
        AppWindow.Show();
        if (AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter { State: Microsoft.UI.Windowing.OverlappedPresenterState.Minimized } p)
            p.Restore();
        Activate();
    }

    private void Quit()
    {
        _quitting = true;
        Close();
        Application.Current.Exit();
    }

    /// <summary>Sizing needs the display scale, which is only known once the window has content.</summary>
    private async void Root_Loaded(object sender, RoutedEventArgs e)
    {
        var scale = Root.XamlRoot.RasterizationScale;
        var screen = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        AppWindow.Resize(new SizeInt32(Math.Min((int)(900 * scale), screen.Width), Math.Min((int)(690 * scale), screen.Height)));
        if (AppWindow.Presenter is OverlappedPresenter window)
        {
            window.PreferredMinimumWidth = (int)(760 * scale);
            window.PreferredMinimumHeight = (int)(560 * scale);
        }
        if (!Prefs.SetupDone || App.HasArg("--setup"))
            await RunSetupAsync();
    }

    /// <summary><c>--settings &lt;page&gt;</c> opens on a given sound-settings page (0 = Cleanup … 3 = Effects).</summary>
    private int? ArgPage()
    {
        var args = Environment.GetCommandLineArgs();
        var i = Array.IndexOf(args, "--settings");
        return i >= 0 && i + 1 < args.Length && int.TryParse(args[i + 1], out var p) ? Math.Clamp(p, 0, _pages.Count - 1) : null;
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var page = (args.SelectedItem as NavigationViewItem)?.Tag;
        foreach (var child in Pages.Children.Cast<FrameworkElement>())
            child.Visibility = Equals(child.Tag, page) ? Visibility.Visible : Visibility.Collapsed;
        PageScroller.ChangeView(null, 0, null, true);
    }

    // ------------------------------------------------------------------ first-run setup

    /// <summary>
    /// WinUI throws if a second ContentDialog opens, and in an async void handler that ends
    /// the app. Assistive tools can still reach buttons behind an open dialog, so ask here.
    /// </summary>
    private async Task<ContentDialogResult> ShowDialogAsync(ContentDialog dialog)
    {
        if (_dialogOpen)
            return ContentDialogResult.None;
        _dialogOpen = true;
        try { return await dialog.ShowAsync(); }
        finally { _dialogOpen = false; }
    }

    private async Task RunSetupAsync()
    {
        var inputs = (IReadOnlyList<string>)InputBox.ItemsSource;
        var cable = ((IReadOnlyList<string>)SendBox.ItemsSource).FirstOrDefault(DefaultMic.IsCable);
        _setup = new SetupDialog(inputs, InputBox.SelectedItem as string, cable is not null, name => InputBox.SelectedItem = name)
        {
            XamlRoot = Root.XamlRoot,
        };
        var finished = await ShowDialogAsync(_setup) == ContentDialogResult.Primary;
        var everyApp = _setup.EveryApp;
        _setup = null;
        Prefs.SetupDone = true;
        if (!finished || cable is null)
            return;
        if (SendBox.SelectedItem is not string send || !DefaultMic.IsCable(send))
            SendBox.SelectedItem = cable;
        DefaultMicSwitch.IsOn = everyApp;
    }

    private async void RunSetup_Click(object sender, RoutedEventArgs e) => await RunSetupAsync();

    // ------------------------------------------------------------------ devices / engine

    private void LoadDevices(DeviceList d)
    {
        _syncing = true;
        var monitors = new List<string> { MonitorOff }.Concat(d.Outputs).ToList();
        InputBox.ItemsSource = d.Inputs;
        SendBox.ItemsSource = d.Outputs;
        MonitorBox.ItemsSource = monitors;
        InputBox.SelectedItem = Choose(_prefs.InputDevice, d.Inputs, d.DefaultInput);
        SendBox.SelectedItem = Choose(_prefs.SendDevice, d.Outputs, d.DefaultSend);
        MonitorBox.SelectedItem = Choose(_prefs.MonitorDevice, monitors, d.DefaultMonitor ?? MonitorOff);
        HearMyself.IsOn = _prefs.MonitorOn;
        _engine.Monitor = _prefs.MonitorOn; // Toggled doesn't fire when the switch already matches
        _syncing = false;
    }

    /// <summary>The device the user picked while it's plugged in, otherwise the best guess.</summary>
    private static string? Choose(string? saved, List<string> available, string? fallback) =>
        saved is not null && available.Contains(saved) ? saved : fallback;

    /// <summary>A device came or went: update the lists, and restart only if that changes our route.</summary>
    private void RefreshDevices()
    {
        var d = Engine.Devices();
        if (d.Inputs.SequenceEqual((List<string>)InputBox.ItemsSource) && d.Outputs.SequenceEqual((List<string>)SendBox.ItemsSource))
            return;
        var route = (InputBox.SelectedItem, SendBox.SelectedItem, MonitorBox.SelectedItem);
        LoadDevices(d);
        if (InputBox.SelectedItem is null || SendBox.SelectedItem is null)
        {
            _engine.Stop();
            RefreshStatus();
        }
        else if (!_stoppedByUser && (!Equals(route, (InputBox.SelectedItem, SendBox.SelectedItem, MonitorBox.SelectedItem)) || !_engine.Status.Running))
        {
            StartEngine();
        }
        RefreshDiscordInfo();
    }

    private void StartEngine()
    {
        if (InputBox.SelectedItem is not string input || SendBox.SelectedItem is not string send)
            return;
        var monitor = MonitorBox.SelectedItem as string;
        _engine.Start(input, send, monitor == MonitorOff ? null : monitor);
        if (_prefs.DefaultMic)
            _defaultMicError = DefaultMic.Apply(_prefs, send); // follow the cable we now send to
        RefreshStatus();
        SyncAppSwitches();
        _tipsShown = "\u0000"; // force the alerts to rebuild (engine errors live there)
    }

    private void Device_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing)
            return;
        // Only the box the user changed: the others stay free to follow the best guess.
        var box = (ComboBox)sender;
        var picked = box.SelectedItem as string;
        if (box == InputBox)
            _prefs.InputDevice = picked;
        else if (box == SendBox)
            _prefs.SendDevice = picked;
        else
            _prefs.MonitorDevice = picked;
        _prefs.Save();
        StartEngine();
    }

    private void StartStop_Click(object sender, RoutedEventArgs e)
    {
        _stoppedByUser = _engine.Status.Running;
        if (_stoppedByUser)
            _engine.Stop();
        else
            StartEngine();
        RefreshStatus();
    }

    private void HearMyself_Toggled(object sender, RoutedEventArgs e)
    {
        _engine.Monitor = HearMyself.IsOn;
        if (_syncing)
            return;
        _prefs.MonitorOn = HearMyself.IsOn;
        _prefs.Save();
    }

    private void RefreshStatus()
    {
        var s = _engine.Status;
        var (text, pill, kind) = StatusLine(s);
        SetPill(EnginePill, pill, kind);
        InputName.Text = DeviceName(InputBox.SelectedItem as string);
        var send = SendBox.SelectedItem as string;
        OutputName.Text = send is not null && DefaultMic.IsCable(send) ? "Virtual Microphone" : DeviceName(send);
        if (!s.Running)
            SetPill(OutputPill, "Not sending", Bad);
        else if (_engine.Muted)
            SetPill(OutputPill, "Muted", Warn);
        else
            SetPill(OutputPill, "Sending", Good);
        if (text != _status)
        {
            _status = text;
            StatusText.Text = text;
            FrameworkElementAutomationPeer.CreatePeerForElement(StatusText).RaiseAutomationEvent(AutomationEvents.LiveRegionChanged);
        }
        _stats["Latency"].Text = s.Running ? $"≈ {s.LatencyMs:0} ms" : "—";
        _stats["Dropouts"].Text = s.Running ? $"{s.Dropouts}" : "—";
        StartStopButton.Content = s.Running ? "Stop" : "Start";
        StartStopButton.Style = s.Running ? null : (Style)Application.Current.Resources["AccentButtonStyle"];
        RecordButton.IsEnabled = s.Running || _engine.RecordingLeft is not null;
        RefreshBoardStatus(s);
    }

    /// <summary>One sentence answering "are people hearing me?", worst problem first.</summary>
    private (string text, string pill, string kind) StatusLine(EngineStatus s)
    {
        if (!s.Running)
        {
            if (InputBox.SelectedItem is null)
                return ("Choose a microphone under Devices", "Stopped", Bad);
            return (_engine.LastError is null ? "Nothing is being sent" : "Audio problem: see the message below", "Stopped", Bad);
        }
        if (_engine.Muted)
            return ("Nothing is being sent while muted", "Muted", Warn);
        if (_meters.Blocks >= 100 && _hold < SilentDb)
            return ("No sound from your mic", "No sound", Bad);
        if (SendBox.SelectedItem is not string send || !DefaultMic.IsCable(send))
            return ($"Sending to {SendBox.SelectedItem}", "Running", Warn);
        if (!_engine.Enhanced)
            return ("Sending your raw mic, without enhancement", "Raw mic", Warn);
        return (_prefs.DefaultMic && _defaultMicError is null
            ? "Apps on Default mic hear your enhanced voice"
            : "Apps using CABLE Output hear your enhanced voice", "Running", Good);
    }

    /// <summary>"Microphone (HyperX Quadcast)" → "HyperX Quadcast".</summary>
    private static string DeviceName(string? name)
    {
        if (name is null)
            return "None";
        var open = name.IndexOf('(');
        return open >= 0 && name.EndsWith(')') ? name[(open + 1)..^1] : name;
    }

    private static void SetPill(Border pill, string text, string kind)
    {
        var resources = Application.Current.Resources;
        pill.Background = (Brush)resources[$"Pill{kind}Background"];
        var label = (TextBlock)pill.Child;
        label.Text = text;
        label.Foreground = (Brush)resources[$"Pill{kind}Foreground"];
    }

    // ------------------------------------------------------------------ A/B, mute, presets

    private void AbTile_Click(object sender, RoutedEventArgs e)
    {
        _engine.ToggleAb();
        RefreshAb();
    }

    private void MuteButton_Click(object sender, RoutedEventArgs e)
    {
        _engine.ToggleMute();
        RefreshAb();
    }

    private void CleanTile_Click(object sender, RoutedEventArgs e) => ToggleSound("ns_enabled");

    private void EqTile_Click(object sender, RoutedEventArgs e) => ToggleSound("eq_enabled");

    private void ToggleSound(string key)
    {
        _engine.Set(key, _engine.Get(key) >= 0.5f ? 0 : 1);
        SoundEdited();
        RefreshAb();
        BuildPage();
    }

    private void PresetTile_Click(object sender, RoutedEventArgs e) => Nav.SelectedItem = PresetsItem;

    private void RefreshAb()
    {
        ShowTile(AbTile, AbPill, "Enhancement", _engine.Enhanced);
        ShowTile(CleanTile, CleanPill, "Noise Removal", _engine.Get("ns_enabled") >= 0.5f);
        ShowTile(EqTile, EqPill, "Equalizer", _engine.Get("eq_enabled") >= 0.5f);
        var muted = _engine.Muted;
        MuteButton.IsChecked = muted;
        MuteText.Text = muted ? "Muted" : "Mute";
        MuteIcon.Glyph = muted ? "" : "";
        RefreshStatus();
    }

    private static void ShowTile(Button tile, Border pill, string name, bool on)
    {
        SetPill(pill, on ? "ON" : "OFF", on ? Good : Off);
        AutomationProperties.SetName(tile, $"{name}: {(on ? "on" : "off")}");
    }

    private void Root_PreviewKeyDown(object sender, KeyRoutedEventArgs e)
    {
        // Don't steal keys from text entry or open dropdowns.
        var focused = FocusManager.GetFocusedElement(Content.XamlRoot);
        if (focused is TextBox or ComboBox)
            return;
        // Tabbed onto a control: Space has to press it. Focus left behind by a mouse click doesn't count.
        if (e.Key == VirtualKey.Space && focused is Control { FocusState: FocusState.Keyboard })
            return;
        if (e.Key == VirtualKey.Space)
        {
            _engine.ToggleAb();
            RefreshAb();
            e.Handled = true;
        }
        else if (e.Key == VirtualKey.M)
        {
            _engine.ToggleMute();
            RefreshAb();
            e.Handled = true;
        }
    }

    /// <summary>Rebuilt whenever a preset is saved or deleted, so it matches the presets folder.</summary>
    private void BuildPresetCards()
    {
        _presetPills.Clear();
        BuiltInPresets.Children.Clear();
        CustomPresets.Children.Clear();
        foreach (var p in _presets)
            BuiltInPresets.Children.Add(PresetCard(p.Key, p.Name, p.Tip));
        var names = _engine.CustomPresets();
        foreach (var name in names)
        {
            var delete = new Button { Content = new FontIcon { Glyph = "\uE74D", FontSize = 14 }, VerticalAlignment = VerticalAlignment.Stretch };
            AutomationProperties.SetName(delete, $"Delete {name}");
            ToolTipService.SetToolTip(delete, "Delete");
            delete.Click += async (_, _) => await DeletePresetAsync(name);
            Grid.SetColumn(delete, 1);
            CustomPresets.Children.Add(new Grid
            {
                ColumnSpacing = 8,
                ColumnDefinitions = { new ColumnDefinition(), new ColumnDefinition { Width = GridLength.Auto } },
                Children = { PresetCard(CustomPrefix + name, name, null), delete },
            });
        }
        NoPresetsText.Visibility = names.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        SyncPreset();
    }

    private Button PresetCard(string key, string name, string? tip)
    {
        var pill = new Border { Style = (Style)Application.Current.Resources["PillStyle"], HorizontalAlignment = HorizontalAlignment.Right, Child = new TextBlock { Style = (Style)Application.Current.Resources["PillTextStyle"] } };
        _presetPills[key] = pill;
        var text = new StackPanel { Spacing = 2, Margin = new Thickness(0, 0, 80, 0), Children = { new TextBlock { Text = name, Style = (Style)Application.Current.Resources["TileTitleStyle"] } } };
        if (tip is not null)
            text.Children.Add(new TextBlock { Text = tip, Style = (Style)Application.Current.Resources["MutedTextStyle"] });
        var card = new Button { Style = (Style)Application.Current.Resources["TileButtonStyle"], Content = new Grid { Children = { text, pill } } };
        AutomationProperties.SetName(card, name);
        card.Click += (_, _) => ApplyPreset(key);
        return card;
    }

    private const string CustomPrefix = "custom:";

    private void SyncPreset()
    {
        var current = _engine.CurrentPreset;
        var picked = _engine.PickedPreset;
        var name = picked.StartsWith(CustomPrefix) ? picked[CustomPrefix.Length..] : _presets.Find(p => p.Key == picked)?.Name;
        foreach (var (key, pill) in _presetPills)
        {
            pill.Visibility = key == current || (current == "" && key == picked) ? Visibility.Visible : Visibility.Collapsed;
            if (key == current)
                SetPill(pill, "Active", Good);
            else
                SetPill(pill, "Edited", Warn);
        }
        // Edited since the last preset: say so, and offer to keep it or go back.
        EditedRow.Visibility = current == "" ? Visibility.Visible : Visibility.Collapsed;
        EditedText.Text = name is null ? "Custom sound" : $"{name} (edited)";
        RevertLink.Visibility = name is null ? Visibility.Collapsed : Visibility.Visible;
        PresetName.Text = current == "" ? EditedText.Text : name;
        AutomationProperties.SetName(PresetTile, $"Preset: {PresetName.Text}");
        ToolTipService.SetToolTip(PresetTile, "Change preset");
    }

    private void Revert_Click(object sender, RoutedEventArgs e)
    {
        _engine.RevertPreset();
        _undo = null;
        SyncPreset();
        RefreshAb();
        BuildPage();
    }

    private void ApplyPreset(string key)
    {
        _engine.ApplyPreset(key);
        _undo = null;
        SyncPreset();
        RefreshAb();
        BuildPage();
    }

    private async void SavePreset_Click(object sender, RoutedEventArgs e)
    {
        var current = _engine.CurrentPreset;
        var box = new TextBox
        {
            PlaceholderText = "e.g. Streaming, Late night, Podcast",
            Text = current.StartsWith(CustomPrefix) ? current[CustomPrefix.Length..] : "",
            MaxLength = 40,
        };
        var error = new TextBlock
        {
            Foreground = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"],
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = "Save as preset",
            PrimaryButtonText = "Save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            Content = new StackPanel
            {
                Spacing = 8,
                Children =
                {
                    new TextBlock { Text = "Saves every sound setting, so you can switch back to this sound in one click. A preset with the same name is replaced.", TextWrapping = TextWrapping.Wrap },
                    box,
                    error,
                },
            },
        };
        dialog.PrimaryButtonClick += (_, args) =>
        {
            if (_engine.SavePreset(box.Text) is { } message)
            {
                error.Text = message;
                error.Visibility = Visibility.Visible;
                args.Cancel = true; // keep the dialog open so the name can be fixed
            }
        };
        if (await ShowDialogAsync(dialog) == ContentDialogResult.Primary)
            BuildPresetCards();
    }

    private async Task DeletePresetAsync(string name)
    {
        var confirm = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Delete “{name}”?",
            Content = "This removes the saved preset. Your current sound stays as it is.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await ShowDialogAsync(confirm) != ContentDialogResult.Primary)
            return;
        _engine.DeletePreset(name);
        BuildPresetCards();
    }

    // ------------------------------------------------------------------ soundboard

    private void BuildSoundboard()
    {
        _syncing = true;
        BoardVolumeSlider.Value = _prefs.BoardVolume * 100;
        BoardMonitorSlider.Value = _prefs.BoardMonitorVolume * 100;
        BoardHotkeysSwitch.IsOn = _prefs.BoardHotkeys;
        StopKeySwitch.IsOn = _prefs.BoardStopKeyOn;
        _syncing = false;
        ShowBoardLevels();
        _hotkeys.Pressed += HotkeyPressed;
        BuildClips();
    }

    /// <summary>Rebuilt whenever a sound or hotkey changes, so it matches the saved list.</summary>
    private void BuildClips()
    {
        _clips = _engine.Clips();
        _clipPills.Clear();
        _clipRows.Clear();
        Clips.Children.Clear();
        foreach (var clip in _clips)
            Clips.Children.Add(ClipRow(clip));
        NoClipsText.Visibility = _clips.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        RegisterHotkeys();
        RefreshPlaying();
    }

    private Grid ClipRow(ClipInfo clip)
    {
        var resources = Application.Current.Resources;
        Border Pill() => new() { Style = (Style)resources["PillStyle"], Visibility = Visibility.Collapsed, Child = new TextBlock { Style = (Style)resources["PillTextStyle"] } };
        var keyPill = Pill();
        var state = Pill();
        _clipPills[clip.Id] = keyPill;
        var icon = new FontIcon { Glyph = PlayGlyph, FontSize = 14, Margin = new Thickness(0, 0, 14, 0) };
        var text = new StackPanel
        {
            Spacing = 2,
            Children =
            {
                new TextBlock { Text = clip.Name, Style = (Style)resources["TileTitleStyle"], TextTrimming = TextTrimming.CharacterEllipsis },
                new TextBlock { Text = $"{clip.Seconds:0.0} s", Style = (Style)resources["MutedTextStyle"] },
            },
        };
        var pills = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(12, 0, 0, 0), Children = { state, keyPill } };
        // Hidden rather than collapsed, so the rows don't jump when a sound starts.
        var bar = new ProgressBar { Maximum = 1, Opacity = 0, Margin = new Thickness(0, 8, 0, 0) };
        Grid.SetColumn(text, 1);
        Grid.SetColumn(pills, 2);
        Grid.SetRow(bar, 1);
        Grid.SetColumnSpan(bar, 3);
        var content = new Grid
        {
            ColumnDefinitions = { new ColumnDefinition { Width = GridLength.Auto }, new ColumnDefinition(), new ColumnDefinition { Width = GridLength.Auto } },
            RowDefinitions = { new RowDefinition { Height = GridLength.Auto }, new RowDefinition { Height = GridLength.Auto } },
            Children = { icon, text, pills, bar },
        };
        var play = new Button { Style = (Style)resources["TileButtonStyle"], Content = content };
        AutomationProperties.SetName(play, $"Play {clip.Name}");
        play.Click += (_, _) => ToggleClip(clip.Id, false);
        _clipRows[clip.Id] = new ClipRowParts(clip, play, icon, state, bar);

        var row = new Grid
        {
            ColumnSpacing = 8,
            ColumnDefinitions = { new ColumnDefinition(), new ColumnDefinition { Width = GridLength.Auto }, new ColumnDefinition { Width = GridLength.Auto }, new ColumnDefinition { Width = GridLength.Auto } },
            Children = { play },
        };
        AddClipButton(row, 1, "\uE7F6", "Preview", $"Preview {clip.Name}: only you hear it", (_, _) => ToggleClip(clip.Id, true));
        AddClipButton(row, 2, "\uE92E", "Set hotkey", $"Set a hotkey for {clip.Name}", async (_, _) => await SetClipKeyAsync(clip));
        AddClipButton(row, 3, "\uE74D", "Delete", $"Delete {clip.Name}", async (_, _) => await DeleteClipAsync(clip));
        return row;
    }

    /// <summary>Shows what's playing right now on each row and on the Stop all card.</summary>
    private void RefreshPlaying()
    {
        var playing = new Dictionary<string, PlayingClip>();
        foreach (var clip in _engine.PlayingClips())
            playing.TryAdd(clip.Id, clip);
        foreach (var (id, row) in _clipRows)
        {
            var on = playing.TryGetValue(id, out var now);
            row.Bar.Opacity = on ? 1 : 0;
            row.State.Visibility = on ? Visibility.Visible : Visibility.Collapsed;
            row.Icon.Glyph = on ? StopGlyph : PlayGlyph;
            row.Icon.Foreground = (Brush)Application.Current.Resources[on ? "AppAccentBrush" : "TextFillColorPrimaryBrush"];
            AutomationProperties.SetName(row.Play, $"{(on ? "Stop" : "Play")} {row.Clip.Name}");
            if (now is null)
                continue;
            row.Bar.Value = Math.Clamp(1 - now.SecondsLeft / row.Clip.Seconds, 0, 1);
            SetPill(row.State, now.Preview ? "Preview" : "Playing", Good);
        }
        _playingCount = playing.Count;
        StopAllButton.IsEnabled = _playingCount > 0;
        if (_playingCount > 0)
            SetPill(PlayingPill, $"{_playingCount} playing", Good);
        else if (DateTime.UtcNow < _stoppedUntil)
            SetPill(PlayingPill, "Stopped", Warn);
        else
            SetPill(PlayingPill, "Nothing playing", Off);
    }

    /// <summary>Stops everything, and says so if there was anything to stop.</summary>
    private void StopAllClips()
    {
        if (_playingCount > 0)
            _stoppedUntil = DateTime.UtcNow + StoppedCue;
        _engine.StopClips();
    }

    private static void AddClipButton(Grid row, int column, string glyph, string tip, string name, RoutedEventHandler click)
    {
        var button = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 14 }, VerticalAlignment = VerticalAlignment.Stretch };
        AutomationProperties.SetName(button, name);
        ToolTipService.SetToolTip(button, tip);
        button.Click += click;
        Grid.SetColumn(button, column);
        row.Children.Add(button);
    }

    private void ToggleClip(string id, bool preview) => ShowBoardProblem(_engine.ToggleClip(id, preview));

    private void ShowBoardProblem(string? problem)
    {
        BoardProblem.Text = problem ?? "";
        BoardProblem.Visibility = problem is null ? Visibility.Collapsed : Visibility.Visible;
    }

    private void StopClips_Click(object sender, RoutedEventArgs e) => StopAllClips();

    private void HotkeyPressed(int id)
    {
        if (id == StopHotkeyId)
            StopAllClips();
        else if (_hotkeyClips.TryGetValue(id, out var clip))
            ShowBoardProblem(_engine.PlayClip(clip, false));
    }

    private void RegisterHotkeys()
    {
        _hotkeys.Clear();
        _hotkeyClips.Clear();
        var on = _prefs.BoardHotkeys;
        var stop = Hotkey.FromPacked(_prefs.BoardStopKey);
        var stopTaken = on && _prefs.BoardStopKeyOn && !_hotkeys.Register(StopHotkeyId, stop);
        StopKeyButton.Content = stopTaken ? $"{stop.Label} (in use)" : stop.Label;
        var id = FirstClipHotkeyId;
        foreach (var clip in _clips)
        {
            var pill = _clipPills[clip.Id];
            pill.Visibility = _prefs.BoardKeys.ContainsKey(clip.Id) ? Visibility.Visible : Visibility.Collapsed;
            if (!_prefs.BoardKeys.TryGetValue(clip.Id, out var packed))
                continue;
            var key = Hotkey.FromPacked(packed);
            if (!on)
            {
                SetPill(pill, key.Label, Off);
            }
            else if (_hotkeys.Register(id, key))
            {
                _hotkeyClips[id] = clip.Id;
                SetPill(pill, key.Label, Good);
                id++;
            }
            else
            {
                SetPill(pill, $"{key.Label} in use", Warn);
            }
        }
    }

    private void BoardLevel_Changed(object sender, RangeBaseValueChangedEventArgs e)
    {
        if (_syncing)
            return;
        _prefs.BoardVolume = BoardVolumeSlider.Value / 100;
        _prefs.BoardMonitorVolume = BoardMonitorSlider.Value / 100;
        _prefs.Save();
        ShowBoardLevels();
    }

    private void ShowBoardLevels()
    {
        _engine.SetBoardLevels(_prefs.BoardVolume, _prefs.BoardMonitorVolume);
        BoardVolumeText.Text = $"{BoardVolumeSlider.Value:0}%";
        BoardMonitorText.Text = $"{BoardMonitorSlider.Value:0}%";
    }

    private void BoardHotkeys_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing)
            return;
        _prefs.BoardHotkeys = BoardHotkeysSwitch.IsOn;
        _prefs.Save();
        RegisterHotkeys();
    }

    private void StopKey_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing)
            return;
        _prefs.BoardStopKeyOn = StopKeySwitch.IsOn;
        _prefs.Save();
        RegisterHotkeys();
    }

    /// <summary>What a press of Play does right now, so it never fails in silence.</summary>
    private void RefreshBoardStatus(EngineStatus s)
    {
        var send = SendBox.SelectedItem as string;
        var text = !s.Running ? "The audio engine is stopped. Start it on the Dashboard to play sounds."
            : _engine.Muted ? "You're muted, so sounds won't play."
            : send is null || !DefaultMic.IsCable(send) ? $"Only {DeviceName(send)} gets these sounds: other apps need the virtual cable. See Devices."
            : MonitorBox.SelectedItem as string == MonitorOff ? "Other people hear your sounds. Choose a Monitor device under Devices to hear them yourself."
            : "Other people hear these sounds alongside your voice.";
        if (text == _boardStatus)
            return;
        _boardStatus = text;
        BoardStatus.Text = text;
    }

    private async void AddSounds_Click(object sender, RoutedEventArgs e)
    {
        var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.MusicLibrary, CommitButtonText = "Add" };
        foreach (var type in new[] { ".mp3", ".wav", ".ogg", ".flac", ".m4a", ".aac" })
            picker.FileTypeFilter.Add(type);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        var files = await picker.PickMultipleFilesAsync();
        if (files.Count == 0)
            return;
        AddSoundsButton.IsEnabled = false;
        var problems = new List<string>();
        foreach (var file in files)
        {
            var path = file.Path;
            var name = file.Name;
            var clip = await Task.Run(() => Engine.PrepareClip(path));
            var problem = clip.Error ?? _engine.AddClip(clip.Id!, clip.Name!);
            if (problem is not null)
                problems.Add($"{name} {problem}.");
        }
        AddSoundsButton.IsEnabled = true;
        ShowBoardProblem(problems.Count == 0 ? null : string.Join(" ", problems));
        BuildClips();
    }

    private async Task DeleteClipAsync(ClipInfo clip)
    {
        var confirm = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Delete “{clip.Name}”?",
            Content = "This removes the sound and its hotkey from the app. Your original file stays where it is.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await ShowDialogAsync(confirm) != ContentDialogResult.Primary)
            return;
        _engine.RemoveClip(clip.Id);
        _prefs.BoardKeys.Remove(clip.Id);
        _prefs.Save();
        BuildClips();
    }

    private async Task SetClipKeyAsync(ClipInfo clip)
    {
        var current = _prefs.BoardKeys.TryGetValue(clip.Id, out var packed) ? Hotkey.FromPacked(packed) : (Hotkey?)null;
        var (result, key) = await AskHotkeyAsync($"Hotkey for “{clip.Name}”", current, true);
        if (result == ContentDialogResult.None)
            return;
        if (result == ContentDialogResult.Secondary)
            _prefs.BoardKeys.Remove(clip.Id);
        else if (key is { } picked)
        {
            if (picked.Packed == _prefs.BoardStopKey)
            {
                ShowBoardProblem($"{picked.Label} already stops all sounds.");
                return;
            }
            // One key, one sound: taking it from another sound beats two sounds at once.
            foreach (var other in _prefs.BoardKeys.Where(kv => kv.Value == picked.Packed).Select(kv => kv.Key).ToList())
                _prefs.BoardKeys.Remove(other);
            _prefs.BoardKeys[clip.Id] = picked.Packed;
        }
        _prefs.Save();
        BuildClips();
    }

    private async void StopKey_Click(object sender, RoutedEventArgs e)
    {
        var (result, key) = await AskHotkeyAsync("Hotkey to stop all sounds", Hotkey.FromPacked(_prefs.BoardStopKey), false);
        if (result != ContentDialogResult.Primary || key is not { } picked)
            return;
        var clash = _prefs.BoardKeys.Where(kv => kv.Value == picked.Packed).Select(kv => kv.Key).ToList();
        if (clash.Count > 0)
        {
            ShowBoardProblem($"{picked.Label} already plays a sound.");
            return;
        }
        _prefs.BoardStopKey = picked.Packed;
        _prefs.Save();
        RegisterHotkeys();
    }

    /// <summary>Asks for a key combination. Primary = use it, Secondary = remove the hotkey.</summary>
    private async Task<(ContentDialogResult result, Hotkey? key)> AskHotkeyAsync(string title, Hotkey? current, bool canRemove)
    {
        if (_dialogOpen)
            return (ContentDialogResult.None, null);
        Hotkey? picked = null;
        var box = new TextBox { IsReadOnly = true, Text = current?.Label ?? "", PlaceholderText = "Press a key combination" };
        AutomationProperties.SetName(box, "Hotkey");
        var note = new TextBlock { Text = "Click the box and press the keys you want.", TextWrapping = TextWrapping.Wrap, Foreground = (Brush)Application.Current.Resources["AppMutedTextBrush"] };
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = title,
            PrimaryButtonText = "Save",
            SecondaryButtonText = canRemove && current is not null ? "Remove" : "",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            IsPrimaryButtonEnabled = false,
            Content = new StackPanel { Spacing = 8, Children = { box, note } },
        };
        box.PreviewKeyDown += (_, e) =>
        {
            var mods = HeldModifiers();
            // Bare Tab and Esc keep moving through and closing the dialog.
            if (Hotkey.IsModifier(e.Key) || (mods == 0 && e.Key is VirtualKey.Tab or VirtualKey.Escape))
                return;
            e.Handled = true;
            var key = new Hotkey(mods, (uint)e.Key);
            var problem = WindowsKeyHeld() ? "Shortcuts with the Windows key belong to Windows." : key.Problem();
            box.Text = key.Label;
            note.Text = problem ?? key.Warning() ?? "Save to use it, or press something else.";
            picked = problem is null ? key : null;
            dialog.IsPrimaryButtonEnabled = problem is null;
        };
        // A registered hotkey never reaches the box: it would play its sound instead.
        _hotkeys.Clear();
        _hotkeyClips.Clear();
        _pickingHotkey = true;
        var result = await ShowDialogAsync(dialog);
        _pickingHotkey = false;
        RegisterHotkeys();
        return (result, picked);
    }

    private static bool KeyDown(VirtualKey key) => InputKeyboardSource.GetKeyStateForCurrentThread(key).HasFlag(CoreVirtualKeyStates.Down);

    private static bool WindowsKeyHeld() => KeyDown(VirtualKey.LeftWindows) || KeyDown(VirtualKey.RightWindows);

    private static uint HeldModifiers() =>
        (KeyDown(VirtualKey.Control) ? Hotkey.Ctrl : 0) | (KeyDown(VirtualKey.Menu) ? Hotkey.Alt : 0) | (KeyDown(VirtualKey.Shift) ? Hotkey.Shift : 0);

    // ------------------------------------------------------------------ sound settings pages

    private void PageBar_SelectionChanged(SelectorBar sender, SelectorBarSelectionChangedEventArgs args)
    {
        _page = Math.Max(0, PageBar.Items.IndexOf(PageBar.SelectedItem));
        _undo = null;
        BuildPage();
    }

    private void ShowAll_Click(object sender, RoutedEventArgs e)
    {
        _prefs.ShowAllSettings = ShowAllBox.IsChecked == true;
        _prefs.Save();
        RefreshDiscordInfo();
        BuildPage();
    }

    private void ResetPage_Click(object sender, RoutedEventArgs e)
    {
        if (_undo is { } saved)
        {
            foreach (var (key, value) in saved)
                _engine.Set(key, value);
            _undo = null;
        }
        else
        {
            _undo = PageValues(_pages[_page]);
            _engine.ResetPage(_page);
        }
        SyncPreset();
        RefreshAb();
        BuildPage();
    }

    private Dictionary<string, float> PageValues(PageInfo page)
    {
        var keys = page.Groups.SelectMany(g => g.Knobs.Select(k => k.Key).Concat(g.Toggle is { } t ? [t] : []));
        return keys.ToDictionary(k => k, _engine.Get);
    }

    /// <summary>The user changed a setting by hand.</summary>
    private void SoundEdited()
    {
        SyncPreset();
        if (_undo is null)
            return;
        _undo = null;
        ResetPageButton.Content = ResetLabel();
    }

    private string ResetLabel() => _undo is null ? $"Reset {_pages[_page].Name.ToLowerInvariant()}" : "Undo reset";

    /// <summary>Generates the visible page from the engine's settings table.</summary>
    private void BuildPage()
    {
        var showAll = _prefs.ShowAllSettings;
        var page = _pages[_page];
        var visibility = showAll ? Visibility.Visible : Visibility.Collapsed;
        PageBar.Visibility = visibility;
        ResetPageButton.Visibility = visibility;
        PageBlurb.Text = showAll ? page.Blurb : SimpleBlurb;
        ResetPageButton.Content = ResetLabel();
        PagePanel.Children.Clear();
        _knobs.Clear();
        var agcOff = _engine.Get("agc_enabled") < 0.5f;

        var groups = showAll ? page.Groups : _pages.SelectMany(p => p.Groups).Where(g => g.Knobs.Any(k => k.Simple));
        foreach (var group in groups)
        {
            var on = group.Toggle is null || _engine.Get(group.Toggle) >= 0.5f;
            if (group.Knobs.Count == 0 && group.Toggle is { } onlyToggle)
            {
                // A plain on/off setting: a single card, no expander.
                var sw = new ToggleSwitch { IsOn = on, OnContent = "On", OffContent = "Off", MinWidth = 0 };
                AutomationProperties.SetName(sw, group.Title);
                sw.Toggled += (_, _) =>
                {
                    _engine.Set(onlyToggle, sw.IsOn ? 1 : 0);
                    SoundEdited();
                };
                PagePanel.Children.Add(new SettingsCard { Header = group.Title, Description = group.Hint, Content = sw });
                continue;
            }
            var expander = new SettingsExpander
            {
                Header = group.Title,
                Description = group.Hint,
                IsExpanded = true,
            };
            if (group.Toggle is { } toggleKey)
            {
                var sw = new ToggleSwitch { IsOn = on, OnContent = "On", OffContent = "Off", MinWidth = 0 };
                AutomationProperties.SetName(sw, group.Title);
                sw.Toggled += (_, _) =>
                {
                    _engine.Set(toggleKey, sw.IsOn ? 1 : 0);
                    SoundEdited();
                    RefreshAb();
                    BuildPage();
                };
                expander.Content = sw;
            }

            foreach (var knob in group.Knobs.Where(k => showAll || k.Simple))
            {
                if (knob.OnlyWhenAgcOff && !agcOff)
                    continue;
                var value = _engine.Get(knob.Key);
                var label = new TextBlock
                {
                    Text = Format.Value(knob.Format, value),
                    MinWidth = 72,
                    TextAlignment = TextAlignment.Right,
                    VerticalAlignment = VerticalAlignment.Center,
                    Foreground = (Brush)Application.Current.Resources["AccentTextFillColorPrimaryBrush"],
                };
                var slider = new Slider
                {
                    Minimum = knob.Min,
                    Maximum = knob.Max,
                    StepFrequency = knob.Step,
                    SmallChange = knob.Step,
                    Value = value,
                    Width = 240,
                    IsThumbToolTipEnabled = false,
                    VerticalAlignment = VerticalAlignment.Center,
                };
                AutomationProperties.SetName(slider, $"{group.Title}: {knob.Label}");
                slider.ValueChanged += (_, e) =>
                {
                    if (_syncing)
                        return;
                    _engine.Set(knob.Key, (float)e.NewValue);
                    label.Text = Format.Value(knob.Format, _engine.Get(knob.Key));
                    SoundEdited();
                };
                var card = new SettingsCard
                {
                    Header = knob.Label,
                    Description = knob.Hint,
                    IsEnabled = on,
                    Content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, Children = { label, slider } },
                };
                expander.Items.Add(card);
                _knobs[knob.Key] = (slider, label, knob);
            }
            PagePanel.Children.Add(new Border { Style = (Style)Application.Current.Resources["CardBackingStyle"], Child = expander });
        }
    }

    // ------------------------------------------------------------------ meters, stats, alerts

    private void BuildStats()
    {
        foreach (var name in new[] { "Voice", "Mic peak", "Output peak", "Room noise", "Auto level", "Smoothing", "Limiter", "Gate", "Latency", "Dropouts" })
        {
            var value = new TextBlock { Text = "—", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
            _stats[name] = value;
            StatsGrid.Children.Add(new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 8,
                Children =
                {
                    new TextBlock { Text = name, Foreground = (Brush)Application.Current.Resources["AppMutedTextBrush"] },
                    value,
                },
            });
        }
    }

    private void BuildWave()
    {
        var brush = (Brush)Application.Current.Resources["AppAccentBrush"];
        for (var i = 0; i < WaveBarCount; i++)
        {
            var bar = new Microsoft.UI.Xaml.Shapes.Rectangle { Width = 3, Height = WaveFloor, RadiusX = 1.5, RadiusY = 1.5, Fill = brush, VerticalAlignment = VerticalAlignment.Center };
            WaveBars.ColumnDefinitions.Add(new ColumnDefinition());
            Grid.SetColumn(bar, i);
            _waveBars.Add(bar);
            WaveBars.Children.Add(bar);
        }
    }

    /// <summary>Scrolls the waveform one bar along, newest level on the right.</summary>
    private void PushWave(float db)
    {
        Array.Copy(_wave, 1, _wave, 0, WaveBarCount - 1);
        _wave[^1] = Fraction(db);
        for (var i = 0; i < WaveBarCount; i++)
            _waveBars[i].Height = WaveFloor + _wave[i] * (WaveBars.Height - WaveFloor);
    }

    private void Tick()
    {
        var (m, hold) = _engine.Poll();
        (_meters, _hold) = (m, hold);
        // Auto level can boost near-silence enough to fool the voice detector.
        var talking = m.Vad > 0.6f && hold >= SilentDb;
        if (++_slowTick % 2 == 0)
        {
            PushWave(m.InPeakDb);
            if (AppWindow.IsVisible && Equals(Nav.SelectedItem, SoundboardItem))
                RefreshPlaying();
        }
        if (_slowTick % 6 != 0)
            return;
        // ~5×/s: remember the sound as it is now (writes only on change), stats, status, alerts, recorder.
        _engine.SaveState();
        var (verdict, kind) = MicVerdict(hold, talking);
        SetPill(LevelPill, verdict, kind);
        _setup?.ShowLevel(Fraction(hold), verdict);
        RoomText.Text = m.NoiseFloorDb < -100 ? "" : m.NoiseFloorDb > -45 ? "Room: noisy" : m.NoiseFloorDb > -55 ? "Room: some noise" : "Room: quiet";
        _stats["Voice"].Text = talking ? "Talking" : "Not talking";
        _stats["Mic peak"].Text = Format.Level(hold);
        _stats["Output peak"].Text = Format.Level(m.OutPeakDb);
        _stats["Auto level"].Text = Format.Value("db", m.AgcGainDb);
        _stats["Smoothing"].Text = Format.Reduction(m.CompGrDb);
        _stats["Limiter"].Text = Format.Reduction(m.LimiterGrDb);
        _stats["Room noise"].Text = m.NoiseFloorDb < -100 ? "—" : $"{m.NoiseFloorDb:0} dBFS";
        _stats["Gate"].Text = Format.Reduction(m.ExpGainDb);
        RefreshStatus();
        RefreshTips();
        RefreshRecorder();
    }

    private static double Fraction(float db) => Math.Clamp((db + 60) / 60.0, 0, 1);

    /// <summary>Below the good range only counts while talking: a quiet room isn't a problem.</summary>
    private static (string text, string kind) MicVerdict(float holdDb, bool talking) =>
        holdDb > ClipDb ? ("Too loud: turn the gain down", Bad)
        : holdDb > HotDb ? ("Nearly too loud", Warn)
        : holdDb >= QuietDb ? ("Good level", Good)
        : holdDb < SilentDb ? ("No sound", Off)
        : talking ? ("Too quiet: move closer", Warn)
        : ("Listening…", Off);

    /// <summary>A condition must hold this long before its warning appears…</summary>
    private static readonly TimeSpan ShowAfter = TimeSpan.FromSeconds(1.5);
    /// <summary>…and the warning stays this long after the condition clears, so it never flickers.</summary>
    private static readonly TimeSpan HideAfter = TimeSpan.FromSeconds(4);
    private const int MaxToasts = 3;
    /// <summary>Warnings that describe the same thing at different severities.</summary>
    private static readonly string[][] ExclusiveAlerts = [["no-signal", "quiet", "clipping"]];

    private readonly Dictionary<string, (DateTime firstSeen, DateTime lastSeen, InfoBarSeverity severity, string title, string message)> _alerts = [];

    private void RefreshTips()
    {
        var now = DateTime.UtcNow;
        var current = new List<(string key, InfoBarSeverity severity, string title, string message)>();
        if (_engine.LastError is { } err)
            current.Add(("error", InfoBarSeverity.Error, "Audio problem", err));
        foreach (var t in _prefs.ShowTips ? _engine.Tips() : new List<TipInfo>())
        {
            var sev = t.Severity switch { "error" => InfoBarSeverity.Error, "warning" => InfoBarSeverity.Warning, _ => InfoBarSeverity.Informational };
            current.Add((t.Key, sev, t.Title, t.Message));
        }
        foreach (var (key, severity, title, message) in current)
        {
            var first = _alerts.TryGetValue(key, out var a) ? a.firstSeen : now;
            _alerts[key] = (first, now, severity, title, message);
            // Alternatives of the same problem replace each other at once (no lingering).
            foreach (var group in ExclusiveAlerts.Where(g => g.Contains(key)))
                foreach (var other in group.Where(k => k != key))
                    _alerts.Remove(other);
        }
        foreach (var key in _alerts.Where(kv => now - kv.Value.lastSeen > HideAfter).Select(kv => kv.Key).ToList())
            _alerts.Remove(key);

        var visible = _alerts
            .Where(kv => !_dismissed.Contains(kv.Key) && (now - kv.Value.firstSeen >= ShowAfter || kv.Key == "error"))
            .OrderByDescending(kv => kv.Value.severity)
            .Take(MaxToasts)
            .ToList();

        // Only touch the visual tree when the set of toasts changes.
        var signature = string.Join("|", visible.Select(kv => kv.Key + kv.Value.message));
        if (signature == _tipsShown)
            return;
        _tipsShown = signature;
        TipsPanel.Children.Clear();
        foreach (var (key, a) in visible)
        {
            var bar = new InfoBar { IsOpen = true, Severity = a.severity, Title = a.title, Message = a.message, IsClosable = true };
            if (key != "error")
            {
                var off = new HyperlinkButton { Content = "Turn off tips", Padding = new Thickness(0) };
                ToolTipService.SetToolTip(off, "Turn them back on under Settings > Pop-up tips");
                off.Click += (_, _) => TipsSwitch.IsOn = false;
                bar.ActionButton = off;
            }
            bar.Closed += (_, _) =>
            {
                _dismissed.Add(key);
                _tipsShown = "\u0000";
            };
            // Floating toast: opaque backing (the InfoBar's own tint is translucent) + shadow,
            // so the page underneath never shows through.
            var toast = new Border
            {
                CornerRadius = new CornerRadius(8),
                Background = (Brush)Application.Current.Resources["SolidBackgroundFillColorBaseBrush"],
                BorderBrush = (Brush)Application.Current.Resources["SurfaceStrokeColorFlyoutBrush"],
                BorderThickness = new Thickness(1),
                Shadow = new ThemeShadow(),
                Translation = new System.Numerics.Vector3(0, 0, 32),
                Child = bar,
            };
            TipsPanel.Children.Add(toast);
        }
    }

    private void RefreshDiscordInfo()
    {
        var cable = SendBox.SelectedItem is string send && DefaultMic.IsCable(send);
        DiscordInfo.IsOpen = cable && !_dismissed.Contains("discord");
        DiscordInfo.Message = (_prefs.DefaultMic
                ? "Leave Discord's Input Device on \"Default\""
                : "Set Discord's Input Device to \"CABLE Output (VB-Audio Virtual Cable)\"")
            + " and turn off Discord's own noise suppression, echo cancellation and automatic gain.";
        // A cable just works, so Send to only needs attention without one.
        var hide = cable && !_prefs.ShowAllSettings;
        SendCard.Visibility = hide ? Visibility.Collapsed : Visibility.Visible;
        SendNote.Visibility = hide ? Visibility.Visible : Visibility.Collapsed;
    }

    private void DiscordInfo_Closed(InfoBar sender, InfoBarClosedEventArgs args) => _dismissed.Add("discord");

    // ------------------------------------------------------------------ app settings

    private void SyncAppSwitches()
    {
        _syncing = true;
        DefaultMicSwitch.IsOn = _prefs.DefaultMic;
        StartupSwitch.IsOn = Prefs.StartWithWindows;
        TipsSwitch.IsOn = _prefs.ShowTips;
        ShowAllBox.IsChecked = _prefs.ShowAllSettings;
        _syncing = false;
        DefaultMicCard.Description = _prefs.DefaultMic && _defaultMicError is { } err
            ? err
            : "Makes the enhanced voice (CABLE Output) the Windows default mic, so apps set to \"Default\" use it. Apps set to a specific mic keep that mic. Turning it on also turns on Start with Windows, so they keep working after a restart. Turning it off, or quitting, puts your previous default mic back.";
        RefreshDiscordInfo();
        RefreshStatus();
    }

    private void DefaultMicSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing)
            return;
        _prefs.DefaultMic = DefaultMicSwitch.IsOn;
        if (_prefs.DefaultMic)
        {
            _defaultMicError = DefaultMic.Apply(_prefs, SendBox.SelectedItem as string);
            // Apps now depend on us being up, so come back after a restart too.
            if (_defaultMicError is null && !Prefs.StartWithWindows)
                Prefs.StartWithWindows = true;
        }
        else
        {
            _defaultMicError = null;
            DefaultMic.Restore(_prefs);
        }
        _prefs.Save();
        SyncAppSwitches();
    }

    private void StartupSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_syncing)
            Prefs.StartWithWindows = StartupSwitch.IsOn;
    }

    private void TipsSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing)
            return;
        _prefs.ShowTips = TipsSwitch.IsOn;
        _prefs.Save();
        _tipsShown = "\u0000"; // rebuild the toasts now
    }

    // ------------------------------------------------------------------ recorder

    private void Record_Click(object sender, RoutedEventArgs e)
    {
        if (_engine.RecordingLeft is not null)
            _engine.StopRecording();
        else
            _engine.StartRecording(RecordSeconds);
        RefreshRecorder();
    }

    private void RefreshRecorder()
    {
        if (_engine.RecordingLeft is { } left)
        {
            RecordButton.Content = $"Stop ({left:0}s)";
            RecordCard.Description = "Recording… talk normally.";
            _wasRecording = true;
            return;
        }
        if (_wasRecording)
        {
            _wasRecording = false;
            _latest = LatestRecording();
        }
        RecordButton.Content = "Record";
        RecordCard.Description = _playError ?? $"Records {RecordSeconds} seconds and saves a raw and an enhanced copy.";
        var saved = _latest is null ? Visibility.Collapsed : Visibility.Visible;
        OpenRecordingsButton.Visibility = saved;
        PlayRawButton.Visibility = saved;
        PlayEnhancedButton.Visibility = saved;
    }

    /// <summary>Newest raw + enhanced pair. File names start with a timestamp, so the last sorts newest.</summary>
    private static (string raw, string enhanced)? LatestRecording()
    {
        var dir = Engine.RecordingsDir();
        if (!Directory.Exists(dir))
            return null;
        var raw = Directory.EnumerateFiles(dir, "*-A-raw.wav").Max(StringComparer.Ordinal);
        var enhanced = raw?.Replace("-A-raw.wav", "-B-enhanced.wav");
        return raw is not null && File.Exists(enhanced) ? (raw, enhanced) : null;
    }

    private void Play_Click(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var stop = _playing == button;
        _player.Pause();
        _player.Source = null;
        if (stop || _latest is not { } files)
        {
            ShowPlaying(null);
            return;
        }
        var file = button == PlayRawButton ? files.raw : files.enhanced;
        _player.Source = MediaSource.CreateFromUri(new Uri(file));
        _player.Play();
        ShowPlaying(button);
    }

    private void ShowPlaying(Button? button, string? error = null)
    {
        _playing = button;
        _playError = error;
        PlayRawButton.Content = button == PlayRawButton ? "Stop" : "Play raw";
        PlayEnhancedButton.Content = button == PlayEnhancedButton ? "Stop" : "Play enhanced";
    }

    private void OpenRecordings_Click(object sender, RoutedEventArgs e)
    {
        var dir = Engine.RecordingsDir();
        System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"\"{dir}\"") { UseShellExecute = true });
    }
}
