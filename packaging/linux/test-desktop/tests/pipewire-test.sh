#!/bin/bash
# Runs the end-to-end audio test against PipeWire (pipewire-pulse + WirePlumber), the sound
# server most current distros ship. Run as root in a fresh ubuntu:24.04 container.
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq --no-install-recommends pipewire pipewire-pulse wireplumber pulseaudio-utils dbus \
    xvfb sox espeak-ng python3 procps libvulkan1 mesa-vulkan-drivers libxkbcommon0 libxkbcommon-x11-0 \
    libwayland-client0 libfontconfig1 libfreetype6 libx11-6 libx11-xcb1 libxcb1 libasound2t64 >/dev/null
echo "missing libraries: $(ldd /target/release/micwright | grep -c 'not found')"
mkdir -p /opt/micwright /config /tests/out && cp /target/release/micwright /opt/micwright/
useradd -m -d /config abc 2>/dev/null || true; chown -R abc /config
su abc -s /bin/bash <<'EOF'
export HOME=/config XDG_RUNTIME_DIR=/tmp/xdg PATH=/opt/micwright:$PATH
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
dbus-daemon --session --address=unix:path=$XDG_RUNTIME_DIR/bus --fork >/dev/null
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus
pipewire >/dev/null 2>&1 & sleep 1; wireplumber >/dev/null 2>&1 & pipewire-pulse >/dev/null 2>&1 & sleep 3
pactl info | grep -E "Server Name"
# A fake mic ("input", fed by paplay) and fake headphones ("output"), like the KDE test desktop.
pactl load-module module-null-sink sink_name=input sink_properties=device.description=input >/dev/null
pactl load-module module-null-sink sink_name=output sink_properties=device.description=output >/dev/null
pactl set-default-sink output; sleep 1
Xvfb :99 -screen 0 1100x800x24 >/dev/null 2>&1 & export DISPLAY=:99; sleep 1
bash /tests/e2e-audio.sh 2>&1 | grep -E "PASS|FAIL|passed|speech"
EOF
