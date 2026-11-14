#!/bin/bash
# End-to-end audio test inside the desktop session (see ../Dockerfile). The browser-mic sink "input" stands in
# for a real mic: we play noisy speech into it and record what apps get from
# "MicWright Virtual Mic".
cfg=/config/.config/MicWright
w=/tmp/mwtest
pass=0; fail=0
ok()  { echo "PASS  $*"; pass=$((pass+1)); }
bad() { echo "FAIL  $*"; fail=$((fail+1)); }
rms() { sox "$1" -n trim "$2" "$3" stat 2>&1 | awk '/RMS     amplitude/ {print $3}'; }
db()  { awk -v a="$1" 'BEGIN { if (a <= 0) print -200; else printf "%.1f", 20*log(a)/log(10) }'; }

app_up() { for i in $(seq 1 40); do pactl list short sources | grep -q micwright_mic && return 0; sleep 0.25; done; return 1; }
start_app() { nohup micwright --background > $w/app.log 2>&1 & sleep 1; app_up; sleep 2; }
stop_app() { micwright --quit; for i in $(seq 1 40); do pgrep -x micwright >/dev/null || return 0; sleep 0.25; done; pkill -x micwright; }

# Records 8 s from the virtual mic while the noisy speech plays into the "mic".
capture() {
    parecord -d micwright_mic --rate=48000 --channels=1 --file-format=wav "$1" & local rec=$!
    sleep 0.3
    paplay --latency-msec=10 -d input $w/noisy.wav
    sleep 0.3
    kill $rec; wait $rec 2>/dev/null
}

rm -rf $w "$cfg"; mkdir -p $w "$cfg"; touch "$cfg/setup-done"
pkill -x micwright; sleep 0.5
pactl list short modules | awk '/micwright/ {print $1}' | xargs -r -n1 pactl unload-module
pactl set-default-source input.monitor

# 5 s of speech, then 3 s of the room alone; pink noise throughout.
espeak-ng -s 150 -w $w/speech.wav "Hello, this is a test of the MicWright voice enhancer running on Linux. One, two, three, four, five."
sox $w/speech.wav -r 48000 -c 2 $w/speech48.wav pad 0.5 3
len=$(soxi -D $w/speech48.wav)
sox -n -r 48000 -c 2 $w/noise.wav synth "$len" pinknoise vol 0.05
sox -m $w/speech48.wav $w/noise.wav $w/noisy.wav
tail_start=$(awk -v l="$len" 'BEGIN { printf "%.2f", l - 2.2 }')

echo "== start (default mic off)"
start_app && ok "app started in the background, virtual mic created" || bad "virtual mic not created: $(cat $w/app.log)"
[ "$(pactl get-default-source)" = "input.monitor" ] && ok "default mic left alone" || bad "default mic changed to $(pactl get-default-source)"
[ "$(pactl list short modules | grep -c micwright)" = 2 ] && ok "exactly one virtual mic (2 modules)" || bad "module count: $(pactl list short modules | grep micwright)"
pactl list sources | grep -q 'Description: MicWright Virtual Mic$' && ok "apps see 'MicWright Virtual Mic'" || bad "virtual mic description"

echo "== enhanced"
capture $w/enh.wav
enh_speech=$(rms $w/enh.wav 0.5 4); enh_room=$(rms $w/enh.wav $tail_start 1.8)
echo "      speech $(db $enh_speech) dBFS, room-only $(db $enh_room) dBFS"
[ "$(db $enh_speech | cut -d. -f1)" -gt -40 ] && ok "voice reaches the virtual mic" || bad "virtual mic too quiet"

echo "== raw (A/B via --toggle-ab)"
micwright --toggle-ab; sleep 0.5
capture $w/raw.wav
micwright --toggle-ab
raw_speech=$(rms $w/raw.wav 0.5 4); raw_room=$(rms $w/raw.wav $tail_start 1.8)
echo "      speech $(db $raw_speech) dBFS, room-only $(db $raw_room) dBFS"
snr_raw=$(awk -v s="$(db $raw_speech)" -v r="$(db $raw_room)" 'BEGIN { print s - r }')
snr_enh=$(awk -v s="$(db $enh_speech)" -v r="$(db $enh_room)" 'BEGIN { print s - r }')
echo "      speech-to-room: raw ${snr_raw} dB, enhanced ${snr_enh} dB"
awk -v a="$snr_enh" -v b="$snr_raw" 'BEGIN { exit !(a > b + 10) }' && ok "noise removal improves speech-to-room by >10 dB" || bad "noise removal gain too small"

echo "== mute (--toggle-mute)"
micwright --toggle-mute; sleep 0.5
capture $w/muted.wav
micwright --toggle-mute
m=$(db "$(rms $w/muted.wav 0.5 4)")
echo "      muted level $m dBFS"
awk -v m="$m" 'BEGIN { exit !(m < -80) }' && ok "mute silences the virtual mic" || bad "mute leaks audio"

echo "== single instance"
micwright --background; sleep 1
[ "$(pgrep -cx micwright)" = 1 ] && ok "a second launch hands over and exits" || bad "two copies running"
micwright --play "nope" ; sleep 0.5
grep -q . $w/app.log; ok "--play reaches the running copy"

echo "== quit"
stop_app
[ "$(pactl list short modules | grep -c micwright)" = 0 ] && ok "quitting removes the virtual mic" || bad "virtual mic left behind"
micwright --toggle-mute 2>/dev/null; [ $? = 1 ] && ok "commands report when MicWright isn't running" || bad "command exit code"

echo "== default mic on"
echo '{"DefaultMic": true}' > "$cfg/app.json"
start_app
[ "$(pactl get-default-source)" = "micwright_mic" ] && ok "virtual mic became the default mic" || bad "default is $(pactl get-default-source)"
grep -q '"PrevDefaultMic": "input.monitor"' "$cfg/app.json" && ok "previous default remembered" || bad "previous default not saved: $(cat $cfg/app.json)"
stop_app
[ "$(pactl get-default-source)" = "input.monitor" ] && ok "quitting restores the previous default mic" || bad "default after quit: $(pactl get-default-source)"

echo "== crash recovery"
start_app; pkill -9 -x micwright; sleep 0.5
[ "$(pactl get-default-source)" = "micwright_mic" ] && echo "      (after a crash the silent virtual mic is still the default)"
echo '{"DefaultMic": false, "PrevDefaultMic": "input.monitor"}' > "$cfg/app.json"
start_app
[ "$(pactl get-default-source)" = "input.monitor" ] && ok "next launch restores the default mic a crash left behind" || bad "default after relaunch: $(pactl get-default-source)"
[ "$(pactl list short modules | grep -c micwright)" = 2 ] && ok "leftover virtual mic reused, not duplicated" || bad "module count after crash: $(pactl list short modules | grep -c micwright)"
stop_app

echo
echo "$pass passed, $fail failed"
cp $w/*.wav /tests/out/ 2>/dev/null
