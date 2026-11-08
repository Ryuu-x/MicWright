# MicWright

Real-time voice enhancement for the **HyperX QuadCast** on Windows. MicWright cleans up your mic as you talk: it removes background noise, evens out your volume, softens harsh "s" sounds and can add effects. Discord, OBS, games and any other app then hear the enhanced voice instead of the raw mic.

Everything runs on your PC. No audio ever goes online.

> MicWright is tuned for the QuadCast but works with any microphone. Pick yours on the **Devices** page.

## Features

- **Noise removal:** fans, keyboards and room hum fade out while your voice stays clear.
- **Auto level** brings a quiet or distant mic up to a steady, broadcast-style loudness.
- **Tone controls:** EQ, de-esser, compressor and warmth.
- **Effects:** pitch shift and reverb.
- **Presets:** Far mic, Quiet room, Noisy room and Beatbox built in, plus your own saved sounds.
- **Soundboard:** play clips into your mic with global hotkeys that work from any app.
- **A/B testing:** press Space to flip between raw and enhanced, and record A/B samples to compare.
- **Use as the Windows default mic,** so every app set to "Default" gets the enhanced voice.
- **Runs from the notification area** and can start with Windows.

## Requirements

- Windows 10 (version 2004 or later) or Windows 11, 64-bit
- A microphone (made for the HyperX QuadCast)
- [VB-CABLE](https://vb-audio.com/Cable/) (free) to send the enhanced voice to other apps

Nothing else is needed: .NET and the Windows App SDK are bundled with the app.

## Download and install

### 1. Install MicWright

1. Open the [Releases](https://github.com/Ryuu-x/MicWright/releases) page and download the latest **`MicWright-<version>-Setup.exe`**.
2. Run it. No administrator rights are needed: it installs just for you, unless you choose "all users".
3. If Windows SmartScreen warns about an unrecognised app, click **More info > Run anyway**. The installer isn't code-signed yet.
4. Pick your options (make the enhanced voice the default mic, start with Windows, desktop icon) and click **Install**.

Prefer no installer? Download **`MicWright-<version>-win-x64.zip`**, unzip it anywhere and run `MicWright.exe`.

### 2. Install VB-CABLE

Windows can't pass one app's audio to another as a microphone on its own. MicWright sends your enhanced voice through a virtual audio cable, and other apps pick it up from there.

1. Download VB-CABLE from VB-Audio's website: <https://vb-audio.com/Cable/>
2. Unzip it, right-click **`VBCABLE_Setup_x64.exe`** > **Run as administrator** > **Install Driver**.
3. Restart your PC.

VB-CABLE is made by VB-Audio and isn't part of MicWright. It's donationware for personal use; business use needs a [paid licence](https://vb-audio.com/Services/licensing.htm).

### 3. First run

The first time it opens, MicWright walks you through setup:

1. **Pick your microphone.**
2. **Check your level** by talking normally, then adjust the mic's gain knob if needed.
3. **Choose where you'll use it:** every app that uses the default mic (recommended), or only the apps you pick.

You can run setup again any time from **Settings > Setup**. Presets for different rooms are on the **Presets** page.

## Use it in Discord, OBS and games

1. In MicWright, open **Devices** and set **Send to** = `CABLE Input (VB-Audio Virtual Cable)`.
2. In your other app, set the microphone to `CABLE Output (VB-Audio Virtual Cable)`.
3. In Discord, also turn **off** its own Noise Suppression, Echo Cancellation and Automatic Gain Control. MicWright already does these, and doing them twice makes your voice sound worse.

**Shortcut:** turn on **Settings > Use as the Windows default mic** instead of step 2. Every app left on "Default" then uses your enhanced voice automatically. Apps that picked the QuadCast by name still get the raw mic, so set those to Default. Turning the switch off, or quitting MicWright, puts your previous default mic back.

With this switch on, closing the window keeps MicWright running in the notification area. Right-click its icon > **Quit** to stop it.

## How it works

MicWright captures your mic at 48 kHz, runs every 10 ms block of audio through a processing chain, and plays the result into VB-CABLE (and your headphones if monitoring is on):

```text
mic ─► high-pass ─► auto level ─► noise removal ─► expander ─► compressor ─► EQ ─► de-esser
    ─► pitch ─► warmth ─► reverb ─► limiter ─► VB-CABLE ─► Discord / OBS / games
                   ▲
             voice detector
```

| Stage | What it does |
|---|---|
| **High-pass** | Cuts low rumble: desk bumps, traffic, air conditioning. |
| **Auto level** | Raises a quiet or distant voice to a steady level. It sits *before* noise removal, because a very quiet voice would otherwise be mistaken for noise. |
| **Noise removal** | DeepFilterNet3 removes background noise and some room echo. RNNoise always runs alongside it as a cheap voice detector. |
| **Expander** | Lowers whatever is left of the background between your words. |
| **Compressor** | Evens out loud and soft parts so you're easy to follow. |
| **EQ** | Shapes your tone: more body, more clarity. |
| **De-esser** | Tames sharp "s" and "sh" sounds. |
| **Pitch, warmth, reverb** | Optional effects. |
| **Limiter** | Stops anything from clipping. |

A **voice detector** follows along and tells auto level and the noise tracker when you're actually speaking, so they don't pump up the room noise between sentences.

The Soundboard mixes your clips in after the voice chain, so others hear them alongside your voice.

## Tips

- Speak into the **side** of the QuadCast (where the HyperX logo is), not the top.
- Set the pattern dial on the back to **cardioid** (the heart icon).
- **Space** flips between raw and enhanced, and **M** mutes.
- **Presets > Save current sound…** keeps a sound you like. Your last settings come back on launch.
- Only add soundboard clips you have the right to share.

## Your files

| What | Where |
|---|---|
| Settings, presets and soundboard sounds | `%APPDATA%\MicWright` |
| A/B recordings | `Music\MicWright` |

Uninstalling MicWright leaves these in place. Delete the folders to remove them.

## Build from source

You'll need:

- [Rust](https://rustup.rs/) (stable, MSVC toolchain)
- [.NET 10 SDK](https://dotnet.microsoft.com/download)
- [Inno Setup 6](https://jrsoftware.org/isinfo.php) to build the installer: `winget install JRSoftware.InnoSetup`

Then, from PowerShell in the repo folder:

```powershell
.\run.ps1      # build and launch the app
.\build.ps1    # tests + release build -> dist\MicWright-<version>-Setup.exe and a portable .zip
```

### Project layout

| Folder | What's in it |
|---|---|
| `crates/mw-dsp` | The voice processing chain: pure Rust, no I/O, real-time safe |
| `crates/mw-engine` | Audio I/O, presets, settings, soundboard; a C interface for the app |
| `crates/mw-app` | Older test app built with GPUI |
| `crates/mw-device`, `crates/mw-ipc`, `crates/mw-dsp-ffi`, `apo` | Experimental system-wide Windows audio effect (APO), not used by the app yet |
| `ui-winui` | The Windows app (WinUI 3, C#) |
| `packaging` | Installer script and the README shipped with the app |

## License

MicWright is released under the [MIT License](LICENSE). Third-party components and their licences are listed in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).

## Trademarks

MicWright is an independent project, not affiliated with or endorsed by HyperX or HP Inc. HyperX and QuadCast are trademarks of HP Inc., used here only to say which microphone the app is made for. VB-CABLE is a product of VB-Audio Software.
