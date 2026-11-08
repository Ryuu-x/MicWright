using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace MicWright;

/// <summary>
/// Command line:
///   --background     start hidden in the tray (used by "Start with Windows")
///   --make-default   turn on "Use as the Windows default mic" (used by the installer)
///   --setup          show the first-run walkthrough, even if it was done before
///   --restore-mic    put the original Windows default mic back and exit (used by the uninstaller)
/// </summary>
public partial class App : Application
{
    private const string InstanceName = @"Local\MicWright.Instance";
    private const string ShowEventName = @"Local\MicWright.Show";

    private MainWindow? _window;
    private Mutex? _instance;

    public App()
    {
        InitializeComponent();
    }

    public static bool HasArg(string name) => Environment.GetCommandLineArgs().Contains(name, StringComparer.OrdinalIgnoreCase);

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (HasArg("--restore-mic"))
        {
            DefaultMic.Restore(Prefs.Load());
            Exit();
            return;
        }

        // One copy only: a second launch (Start menu while it sits in the tray) shows the first.
        _instance = new Mutex(true, InstanceName, out var first);
        var show = new EventWaitHandle(false, EventResetMode.AutoReset, ShowEventName);
        if (!first)
        {
            show.Set();
            Exit();
            return;
        }

        _window = new MainWindow();
        if (!HasArg("--background"))
            _window.Activate();

        var dispatcher = DispatcherQueue.GetForCurrentThread();
        new Thread(() =>
        {
            while (show.WaitOne())
                dispatcher.TryEnqueue(() => _window.ShowFromTray());
        }) { IsBackground = true, Name = "single-instance" }.Start();
    }
}
