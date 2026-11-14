# A KDE Plasma (Wayland) desktop in the browser for trying the Linux build by hand:
#   docker build -t micwright-desktop packaging/linux/test-desktop
#   docker run -d --name micwright-desktop -p 3001:3001 --shm-size=1gb \
#     -v mw-target:/target:ro -v "$PWD/packaging/linux/test-desktop/tests:/tests" micwright-desktop
# then open https://localhost:3001. The app is built into the `mw-target` volume by
# packaging/linux/build.sh and installed at /opt/micwright on container start.
#
# Automated checks (the browser mic sink "input" stands in for a real mic):
#   docker exec -u abc micwright-desktop insession bash /tests/e2e-audio.sh
# UI automation runs on a private Xvfb display (source tests/ui-lib.sh), because
# XWayland drops synthetic input. tests/pipewire-test.sh repeats the audio checks on
# PipeWire in a plain ubuntu:24.04 container.
FROM lscr.io/linuxserver/webtop:ubuntu-kde

# Test tools: synthetic speech, recording from the virtual mic, driving and capturing
# the window (on Xvfb), and reading D-Bus.
RUN apt-get update && apt-get install -y --no-install-recommends \
        espeak-ng pulseaudio-utils sox xdotool x11-apps x11-utils imagemagick xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY insession /usr/local/bin/insession
COPY 50-micwright /custom-cont-init.d/50-micwright
COPY micwright.desktop /usr/share/applications/io.github.ryuu_x.MicWright.desktop
RUN chmod +x /usr/local/bin/insession /custom-cont-init.d/50-micwright
