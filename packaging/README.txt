MicWright
=========

Real-time voice enhancement for the HyperX QuadCast: noise removal
(on your PC, nothing goes online), auto level, smoothing, EQ, ess control, and effects
(pitch, warmth, reverb). Runs on Windows 10 (1903+) and Windows 11, x64.


Start it
--------
Run MicWright-<version>-Setup.exe (no admin needed), or unzip the
portable folder anywhere and run MicWright.exe. Nothing else needs
installing: .NET and the Windows App SDK are included.


Use it in Discord, OBS, games...
--------------------------------
Windows needs a "virtual cable" to carry the enhanced voice to other apps.

1. Install VB-CABLE from VB-Audio's website: https://vb-audio.com/Cable/
   Unzip it, right-click VBCABLE_Setup_x64.exe > Run as administrator >
   Install Driver, then restart your PC.
   VB-CABLE (www.vb-cable.com) isn't part of this app. It is donationware,
   all participations are welcome. Business use needs a paid licence:
   https://vb-audio.com/Services/licensing.htm
2. In MicWright: Devices > Send to = "CABLE Input (VB-Audio Virtual Cable)".
3. In Discord / OBS / your game: microphone = "CABLE Output (VB-Audio
   Virtual Cable)". In Discord also turn OFF its own Noise Suppression,
   Echo Cancellation and Automatic Gain Control.

Or skip step 3 for apps set to Default: turn on Settings > "Use as the Windows
default mic". That makes CABLE Output the Windows default mic, so any app left on
"Default" uses your enhanced voice. The app then keeps running in the
notification area when you close its window (right-click the icon > Quit to
stop it); quitting or turning the switch off puts your previous default mic back.
Apps that picked "Microphone (HyperX Quadcast)" by name still get the raw
mic: set them to Default.


Soundboard
----------
Add short sounds (MP3, WAV, OGG, FLAC, M4A, up to a minute each) on the
Soundboard page, then click one to start or stop it, or give it a hotkey. Others hear them alongside
your voice, as long as you send through the virtual cable above. The headphone
button previews a sound for you alone. Muting stops sounds too. Hotkeys work
from any app, and the Stop all hotkey cuts off whatever is playing. Only use sounds you have
the right to share.


Tips
----
- Speak into the SIDE of the QuadCast (the HyperX logo), not the top.
- Set the dial on the back to cardioid (heart icon).
- Space flips between raw and enhanced (A/B), M mutes.
- Presets > Save current sound keeps your sound; your last settings are
  restored on launch.


Your files
----------
Settings and presets:  %APPDATA%\MicWright
A/B recordings:        Music\MicWright


Trademarks
----------
MicWright is an independent project, not affiliated with or endorsed by
HyperX or HP Inc. HyperX and QuadCast are trademarks of HP Inc., used here
only to say which microphone the app is made for.
