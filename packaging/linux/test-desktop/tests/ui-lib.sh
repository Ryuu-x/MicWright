#!/bin/bash
# Helpers for driving MicWright's window under XWayland.
out=/tests/out
wid() { xdotool search --name "^MicWright$" | head -1; }
shot() { sleep "${2:-0.8}"; xwd -silent -id "$(wid)" | convert xwd:- "$out/$1.png"; echo "shot $1"; }
click() { xdotool mousemove --window "$(wid)" "$1" "$2"; sleep 0.15; xdotool click 1; sleep "${3:-0.5}"; }
key() { xdotool key "$@"; sleep 0.4; }
type_text() { xdotool type --delay 40 "$1"; sleep 0.3; }
nav() {  # sidebar entries, top to bottom
    case $1 in
        dashboard) click 99 70 ;; sound) click 99 106 ;; presets) click 99 142 ;;
        soundboard) click 99 178 ;; devices) click 99 214 ;; settings) click 99 250 ;;
    esac
}
# Speech on a loop into the test mic, so meters and the voice detector have something.
talk_start() {
    [ -f /tmp/mwtest/noisy.wav ] || return
    ( while true; do paplay --latency-msec=10 -d input /tmp/mwtest/noisy.wav; done ) & echo $! > /tmp/talk.pid
}
talk_stop() { [ -f /tmp/talk.pid ] && { pkill -P "$(cat /tmp/talk.pid)"; kill "$(cat /tmp/talk.pid)"; rm /tmp/talk.pid; } 2>/dev/null; }
# A private X server for automation: XWayland drops synthetic input, Xvfb doesn't.
xvfb_start() {
    pgrep -f "Xvfb :99" >/dev/null || { Xvfb :99 -screen 0 1100x800x24 -nolisten tcp >/dev/null 2>&1 & sleep 1; }
    export DISPLAY=:99; unset WAYLAND_DISPLAY
}
app_start() { pkill -x micwright; sleep 0.5; nohup micwright "$@" > /tmp/gui.log 2>&1 & for i in $(seq 1 40); do [ -n "$(wid)" ] && break; sleep 0.25; done; sleep 2; }
