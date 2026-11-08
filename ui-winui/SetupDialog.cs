using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace MicWright;

/// <summary>First-run walkthrough: pick the mic, check the level, choose where the voice goes.</summary>
public sealed class SetupDialog : ContentDialog
{
    private const int LastStep = 2;

    private readonly StackPanel[] _steps;
    private readonly string[] _titles = ["Choose your microphone", "Check your level", "Where will you use it?"];
    private readonly ProgressBar _level = new() { Maximum = 1, Height = 8 };
    private readonly TextBlock _verdict = new() { Text = "Listening…" };
    private readonly RadioButtons _use = new() { SelectedIndex = 0 };
    private int _step;

    /// <summary>The user chose to make the enhanced voice the Windows default mic.</summary>
    public bool EveryApp => _use.SelectedIndex == 0;

    public SetupDialog(IReadOnlyList<string> inputs, string? input, bool hasCable, Action<string> pickInput)
    {
        var mics = new ComboBox { ItemsSource = inputs, SelectedItem = input, HorizontalAlignment = HorizontalAlignment.Stretch };
        AutomationProperties.SetName(mics, "Microphone");
        mics.SelectionChanged += (_, _) =>
        {
            if (mics.SelectedItem is string name)
                pickInput(name);
        };
        AutomationProperties.SetName(_level, "Mic level");
        AutomationProperties.SetLiveSetting(_verdict, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);

        var useStep = new StackPanel { Spacing = 12, Children = { Text("Your enhanced voice reaches other apps through a virtual cable.") } };
        if (hasCable)
        {
            _use.Items.Add(Choice("Apps using the default mic (recommended)", "Makes the enhanced voice the Windows default mic, so apps set to Default use it. The app also starts with Windows, so this keeps working after a restart."));
            _use.Items.Add(Choice("Only the apps I choose", "Pick CABLE Output as the mic in Discord, OBS or your game. Your Windows default mic stays as it is."));
            useStep.Children.Add(_use);
        }
        else
        {
            _use.SelectedIndex = 1;
            useStep.Children.Add(new InfoBar
            {
                IsOpen = true,
                IsClosable = false,
                Severity = InfoBarSeverity.Warning,
                Title = "VB-CABLE is needed",
                Message = "Install it from VB-Audio's website and restart your PC, then open Settings and choose Run setup again. VB-CABLE is donationware, and business use needs a paid licence.",
                ActionButton = new HyperlinkButton { Content = "Get VB-CABLE", NavigateUri = new Uri("https://vb-audio.com/Cable/") },
            });
        }

        _steps =
        [
            new StackPanel { Spacing = 12, Children = { Text("This is the mic the app listens to."), mics } },
            new StackPanel { Spacing = 12, Children = { Text("Say a sentence at your normal volume."), _level, _verdict } },
            useStep,
        ];
        var content = new Grid { MinWidth = 420 };
        foreach (var step in _steps)
            content.Children.Add(step);
        Content = content;

        CloseButtonText = "Skip";
        DefaultButton = ContentDialogButton.Primary;
        PrimaryButtonClick += (_, e) =>
        {
            if (_step == LastStep)
                return;
            e.Cancel = true;
            Show(_step + 1);
        };
        SecondaryButtonClick += (_, e) =>
        {
            e.Cancel = true;
            Show(_step - 1);
        };
        Show(0);
    }

    /// <summary>Live mic level; <paramref name="fraction"/> runs from 0 to 1.</summary>
    public void ShowLevel(double fraction, string verdict)
    {
        _level.Value = fraction;
        _verdict.Text = verdict;
    }

    private void Show(int step)
    {
        _step = step;
        Title = $"{_titles[step]} ({step + 1} of {_steps.Length})";
        for (var i = 0; i < _steps.Length; i++)
            _steps[i].Visibility = i == step ? Visibility.Visible : Visibility.Collapsed;
        PrimaryButtonText = step == LastStep ? "Finish" : "Next";
        SecondaryButtonText = step == 0 ? "" : "Back";
    }

    private static TextBlock Text(string text) => new() { Text = text, TextWrapping = TextWrapping.Wrap };

    private static RadioButton Choice(string title, string hint)
    {
        var choice = new RadioButton
        {
            Content = new StackPanel
            {
                Children =
                {
                    new TextBlock { Text = title },
                    new TextBlock
                    {
                        Text = hint,
                        TextWrapping = TextWrapping.Wrap,
                        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
                    },
                },
            },
        };
        AutomationProperties.SetName(choice, title);
        return choice;
    }
}
