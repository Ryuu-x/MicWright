MicWright for Linux (beta)
==========================

Real-time voice enhancement for your microphone. MicWright removes background
noise, evens out your volume and can add effects, and other apps (Discord, OBS,
games) hear the enhanced voice. Everything runs on your PC.

Requirements
------------
- 64-bit Linux with PipeWire or PulseAudio (every current desktop has one)
- Vulkan graphics (Mesa or your GPU driver)
- Nothing else: no VB-CABLE. MicWright makes its own virtual microphone.

Install
-------
    ./install.sh

Then start MicWright from your app menu. To remove it: ./install.sh --uninstall
You can also run ./micwright straight from this folder without installing.

Use it in Discord, OBS and games
--------------------------------
Pick "MicWright Virtual Mic" as the microphone in the other app. In Discord,
also turn off its own Noise Suppression, Echo Cancellation and Automatic Gain
Control.

Or turn on Settings > Use as the default mic: apps set to "Default" then get
the enhanced voice. Quitting MicWright puts your previous default mic back.

Soundboard hotkeys
------------------
- X11 desktops: hotkeys work everywhere.
- Wayland: KDE Plasma, GNOME 48 or later and Hyprland ask you once to allow
  them. Other Wayland desktops don't let apps have global hotkeys; bind these
  commands to keys in your desktop's keyboard settings instead:
      micwright --play "Sound name"
      micwright --stop-sounds
      micwright --toggle-mute

Tray icon
---------
KDE and most desktops show it. GNOME needs the "AppIndicator and KStatusNotifierItem
Support" extension.

Your files
----------
Settings, presets and sounds   ~/.config/MicWright
A/B recordings                 ~/Music/MicWright

Licence
-------
MicWright is MIT licensed (LICENSE). Third-party components and their
licences are listed in THIRD-PARTY-NOTICES.txt.
