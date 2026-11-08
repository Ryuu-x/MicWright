//! MicWright — test mode, built with GPUI (Zed's GPU-accelerated UI) via gpui-kit.
//!
//! Hear the chain on your own voice before installing anything system-wide:
//! mic → mw-dsp → headphones, with an instant A/B (bypass) toggle, presets,
//! live meters and a raw-vs-enhanced recorder.

#![windows_subsystem = "windows"]

use mw_engine::{audio, settings};

use audio::{Engine, Recording, Shared};
use settings::{all_knobs, page_offset, PAGES};
use gpui_kit::component::alert::{Alert, AlertVariant};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, Sizable as _, Theme, ThemeMode};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use mw_dsp::{Meters, Params};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RECORD_SECONDS: usize = 15;

actions!(micwright, [ToggleAb, ToggleMute]);

fn main() {
    if std::env::args().any(|a| a == "--selftest") {
        std::process::exit(selftest());
    }
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.bind_keys([KeyBinding::new("space", ToggleAb, None), KeyBinding::new("m", ToggleMute, None)]);
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("MicWright".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(620.), px(860.)), cx))),
            window_min_size: Some(size(px(480.), px(560.))),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| MicWrightApp::new(window, cx))).expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Preset {
    Far,
    Quiet,
    Noisy,
    Beatbox,
}

impl Preset {
    const ALL: [(Preset, &'static str, &'static str); 4] = [
        (Preset::Far, "Far mic", "Mic is some distance away: strong denoise + auto-gain"),
        (Preset::Quiet, "Quiet room", "Lighter processing for a quiet room or close mic"),
        (Preset::Noisy, "Noisy room", "Loud fans or keyboard: stronger gating"),
        (Preset::Beatbox, "Beatbox", "Keeps every sound: no denoise, gating or de-essing"),
    ];

    fn params(self) -> Params {
        match self {
            Preset::Far => Params::far_mic(),
            Preset::Quiet => Params::quiet_room(),
            Preset::Noisy => Params::noisy_room(),
            Preset::Beatbox => Params::beatbox(),
        }
    }
}

// ---------------------------------------------------------------- app state

type DeviceSelect = SelectState<Vec<SharedString>>;

const MONITOR_OFF: &str = "Off (don't play back)";

struct MicWrightApp {
    shared: Arc<Shared>,
    params: Params,
    preset: Option<Preset>,
    /// Last preset picked; "Reset" restores its values.
    base_preset: Preset,
    input: String,
    /// Where the enhanced voice goes for other apps (e.g. "CABLE Input" → Discord).
    send: String,
    /// Optional headphones to hear yourself on.
    monitor: Option<String>,
    input_select: Entity<DeviceSelect>,
    send_select: Entity<DeviceSelect>,
    monitor_select: Entity<DeviceSelect>,
    engine: Option<Engine>,
    error: Option<String>,
    meters: Meters,
    in_peak_hold: f32,
    recording_until: Option<Instant>,
    last_saved: Option<PathBuf>,
    /// One per knob, in `all_knobs()` order.
    sliders: Vec<Entity<SliderState>>,
    show_advanced: bool,
    settings_page: usize,
    /// Tips the user closed (by key); they stay hidden for this session.
    dismissed: HashSet<&'static str>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
}

impl MicWrightApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let params = Params::far_mic();
        let (din, dsend, dmonitor) = audio::default_devices();
        let input = din.unwrap_or_default();
        let send = dsend.unwrap_or_default();
        let monitor = dmonitor;

        let mut subs = Vec::new();
        let input_select = device_select(audio::input_names(), &input, window, cx);
        let send_select = device_select(audio::output_names(), &send, window, cx);
        let monitor_names: Vec<String> = std::iter::once(MONITOR_OFF.to_string()).chain(audio::output_names()).collect();
        let monitor_select = device_select(monitor_names, monitor.as_deref().unwrap_or(MONITOR_OFF), window, cx);
        subs.push(cx.subscribe_in(&input_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                this.input = name.to_string();
                this.start();
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&send_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                this.send = name.to_string();
                this.start();
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&monitor_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                this.monitor = (name.as_ref() != MONITOR_OFF).then(|| name.to_string());
                this.start();
                cx.notify();
            }
        }));

        let mut sliders = Vec::new();
        for knob in all_knobs() {
            let state = cx.new(|_| SliderState::new().min(knob.min).max(knob.max).step(knob.step).default_value((knob.get)(&params)));
            let set = knob.set;
            subs.push(cx.subscribe(&state, move |this, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(v) = ev else { return };
                set(&mut this.params, v.start());
                this.preset = None;
                this.push_params();
                cx.notify();
            }));
            sliders.push(state);
        }

        // Meters refresh at ~30 fps.
        let ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(33)).await;
            if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                break;
            }
        });

        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let mut app = Self {
            shared: Shared::new(params),
            params,
            preset: Some(Preset::Far),
            base_preset: Preset::Far,
            input,
            send,
            monitor,
            input_select,
            send_select,
            monitor_select,
            engine: None,
            error: None,
            meters: Meters::default(),
            in_peak_hold: -90.0,
            recording_until: None,
            last_saved: None,
            sliders,
            // `--settings <page>` opens straight onto a settings page (0 = Cleanup … 3 = Effects).
            show_advanced: settings_arg().is_some(),
            settings_page: settings_arg().unwrap_or(0).min(PAGES.len() - 1),
            dismissed: HashSet::new(),
            focus,
            _subscriptions: subs,
            _ticker: ticker,
        };
        app.start();
        app
    }

    fn start(&mut self) {
        self.engine = None;
        match Engine::start(&self.input, &self.send, self.monitor.as_deref(), self.shared.clone()) {
            Ok(e) => {
                self.engine = Some(e);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Ok(m) = self.shared.meters.try_lock() {
            self.meters = *m;
        }
        self.in_peak_hold = (self.in_peak_hold - 0.5).max(self.meters.in_peak_db);
        if self.recording_until.is_some_and(|t| Instant::now() >= t) {
            self.finish_recording();
        }
        cx.notify();
    }

    fn push_params(&self) {
        self.shared.set_params(self.params);
    }

    /// Room presets change cleanup and level; your tone and effects pages are kept.
    fn apply_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.params;
        let mut p = preset.params();
        (p.enabled, p.mute) = (old.enabled, old.mute);
        settings::reset_page(2, &mut p, &old);
        settings::reset_page(3, &mut p, &old);
        self.params = p;
        self.preset = Some(preset);
        self.base_preset = preset;
        self.push_params();
        self.sync_sliders(window, cx);
    }

    fn reset_settings_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = self.base_preset.params();
        settings::reset_page(page, &mut self.params, &defaults);
        self.push_params();
        self.sync_sliders(window, cx);
    }

    fn sync_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let params = self.params;
        for (state, knob) in self.sliders.iter().zip(all_knobs()) {
            state.update(cx, |s, cx| s.set_value((knob.get)(&params), window, cx));
        }
        cx.notify();
    }

    fn toggle_ab(&mut self, _: &ToggleAb, _: &mut Window, cx: &mut Context<Self>) {
        self.params.enabled ^= 1;
        self.push_params();
        cx.notify();
    }

    fn toggle_mute(&mut self, _: &ToggleMute, _: &mut Window, cx: &mut Context<Self>) {
        self.params.mute ^= 1;
        self.push_params();
        cx.notify();
    }

    fn start_recording(&mut self) {
        let n = RECORD_SECONDS * mw_dsp::SAMPLE_RATE as usize;
        *self.shared.recording.lock().unwrap() = Some(Recording { raw: Vec::with_capacity(n), processed: Vec::with_capacity(n) });
        self.recording_until = Some(Instant::now() + Duration::from_secs(RECORD_SECONDS as u64));
    }

    fn finish_recording(&mut self) {
        self.recording_until = None;
        let Some(rec) = self.shared.recording.lock().unwrap().take() else { return };
        let dir = recordings_dir();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let raw = dir.join(format!("{stamp}-A-raw.wav"));
        let enh = dir.join(format!("{stamp}-B-enhanced.wav"));
        match audio::write_wav(&raw, &rec.raw).and_then(|_| audio::write_wav(&enh, &rec.processed)) {
            Ok(()) => self.last_saved = Some(dir),
            Err(e) => self.error = Some(format!("saving recording failed: {e:#}")),
        }
    }

    fn tips(&self) -> Vec<Tip> {
        let m = self.meters;
        let mut tips = Vec::new();
        if self.engine.is_none() || m.blocks < 100 {
            return tips;
        }
        if self.in_peak_hold < -60.0 {
            tips.push(Tip {
                key: "no-signal",
                variant: AlertVariant::Error,
                title: "No signal from the mic",
                message: "Is it muted (red LED off)? Tap the top of the QuadCast to unmute, and check the gain knob underneath isn't at zero.",
            });
        } else if self.in_peak_hold < -35.0 && m.vad > 0.6 {
            tips.push(Tip {
                key: "quiet",
                variant: AlertVariant::Warning,
                title: "Your voice is very quiet at the mic",
                message: "Speak into the SIDE with the HyperX logo (not the top), turn the gain knob up, and set the Windows mic level to 100.",
            });
        }
        if self.in_peak_hold > -1.0 {
            tips.push(Tip {
                key: "clipping",
                variant: AlertVariant::Error,
                title: "The mic is clipping",
                message: "Turn the gain knob underneath the mic down a little.",
            });
        }
        if self.params.agc_enabled != 0 && m.agc_gain_db > self.params.agc_max_gain_db - 2.0 {
            tips.push(Tip {
                key: "agc-limit",
                variant: AlertVariant::Warning,
                title: "Auto level is at its limit",
                message: "Your voice needs more boost than allowed. Moving the mic closer sounds far better than more gain.",
            });
        }
        if m.noise_floor_db > -45.0 {
            tips.push(Tip {
                key: "noisy",
                variant: AlertVariant::Info,
                title: "Noisy room",
                message: "Try the 'Noisy room' preset, and set the dial on the back of the mic to cardioid (heart icon).",
            });
        }
        tips.retain(|t| !self.dismissed.contains(t.key));
        tips
    }
}

struct Tip {
    key: &'static str,
    variant: AlertVariant,
    title: &'static str,
    message: &'static str,
}

fn device_select(names: Vec<String>, current: &str, window: &mut Window, cx: &mut Context<MicWrightApp>) -> Entity<DeviceSelect> {
    let items: Vec<SharedString> = names.into_iter().map(SharedString::from).collect();
    let index = items.iter().position(|n| n.as_ref() == current).map(|i| IndexPath::default().row(i));
    cx.new(|cx| SelectState::new(items, index, window, cx))
}

fn settings_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--settings")?;
    Some(args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn recordings_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    base.join("Music").join("MicWright")
}

// ---------------------------------------------------------------- rendering

impl Render for MicWrightApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("root")
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_ab))
            .on_action(cx.listener(Self::toggle_mute))
            .bg(theme.background)
            .text_color(theme.foreground)
            .overflow_y_scroll()
            .p_5()
            .gap_4()
            .child(self.render_header(cx))
            .child(self.render_devices(cx))
            .child(self.render_ab(cx))
            .child(self.render_meters(cx))
            .children(self.render_tips(cx))
            .child(self.render_recorder(cx))
            .child(self.render_advanced(cx))
    }
}

impl MicWrightApp {
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_2xl().font_weight(FontWeight::BOLD).child("MicWright"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Test mode: mic → MicWright → virtual cable (Discord, OBS…) + your headphones. Nothing is installed system-wide."),
            )
    }

    fn render_devices(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let running = self.engine.is_some();
        let dropouts = self.shared.underruns.load(Ordering::Relaxed) + self.shared.overflows.load(Ordering::Relaxed);
        let monitor = self.shared.monitor.load(Ordering::Relaxed);
        let speakers = |n: &str| {
            let l = n.to_lowercase();
            !audio::is_virtual_cable(n) && !l.contains("quadcast") && !l.contains("headphone") && !l.contains("headset")
        };
        let feedback_risk = speakers(&self.send) || (monitor && self.monitor.as_deref().is_some_and(speakers));
        let send_is_cable = audio::is_virtual_cable(&self.send);

        card(cx)
            .child(labeled("Microphone", Select::new(&self.input_select).id("input-select")))
            .child(labeled("Send to", Select::new(&self.send_select).id("send-select")))
            .child(labeled("Monitor", Select::new(&self.monitor_select).id("monitor-select")))
            .child(
                h_flex()
                    .gap_2()
                    .child(if running {
                        Button::new("stop").label("Stop").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.engine = None;
                            cx.notify();
                        }))
                    } else {
                        Button::new("start").primary().label("Start").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.start();
                            cx.notify();
                        }))
                    })
                    .child(Switch::new("monitor").checked(monitor).label("Hear myself").on_click(cx.listener(
                        |this, checked: &bool, _, cx| {
                            this.shared.monitor.store(*checked, Ordering::Relaxed);
                            cx.notify();
                        },
                    ))),
            )
            .when_some(self.engine.as_ref(), |el, e| {
                el.child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                    "Running · latency ≈ {:.0} ms · dropouts {dropouts} · {} → {}",
                    e.chain_latency_ms, e.input_desc, e.output_desc
                )))
            })
            .when_some(self.error.clone(), |el, err| el.child(Alert::error("engine-error", err).title("Audio problem")))
            .when(send_is_cable && !self.dismissed.contains("discord"), |el| {
                el.child(
                    Alert::info(
                        "discord-hint",
                        "Set Discord's Input Device to \"CABLE Output (VB-Audio Virtual Cable)\" and turn off Discord's own noise suppression, echo cancellation and automatic gain.",
                    )
                    .title("Using this with Discord")
                    .on_close(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismissed.insert("discord");
                        cx.notify();
                    })),
                )
            })
            .when(feedback_risk, |el| {
                el.child(Alert::warning("feedback", "Monitoring through speakers will feed back into the mic.").title("Use headphones"))
            })
    }

    fn render_ab(&self, cx: &Context<Self>) -> impl IntoElement {
        let enhanced = self.params.enabled != 0;
        let muted = self.params.mute != 0;
        card(cx)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("ab")
                            .large()
                            .w(px(260.))
                            .label(if enhanced { "ENHANCED (B)" } else { "RAW MIC (A)" })
                            .when(enhanced, |b| b.success())
                            .tooltip("Click or press Space to compare raw vs enhanced")
                            .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.toggle_ab(&ToggleAb, w, cx))),
                    )
                    .child(
                        Button::new("mute")
                            .large()
                            .label(if muted { "Muted" } else { "Live" })
                            .when(muted, |b| b.danger())
                            .tooltip("Mute the enhanced output (M)")
                            .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.toggle_mute(&ToggleMute, w, cx))),
                    ),
            )
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Press Space to flip between A and B while you talk."))
            .child(
                h_flex().gap_2().items_center().child(div().text_sm().child("Preset")).children(Preset::ALL.iter().map(|&(p, name, tip)| {
                    Button::new(SharedString::from(format!("preset-{name}")))
                        .small()
                        .label(name)
                        .tooltip(tip)
                        .selected(self.preset == Some(p))
                        .when(self.preset == Some(p), |b| b.primary())
                        .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| this.apply_preset(p, w, cx)))
                })),
            )
    }

    fn render_meters(&self, cx: &Context<Self>) -> impl IntoElement {
        let m = self.meters;
        card(cx)
            .child(meter("Mic input", m.in_peak_db, Some(self.in_peak_hold), cx))
            .child(meter("Output", m.out_peak_db, None, cx))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_6()
                    .gap_y_2()
                    .child(stat("Voice", if m.vad > 0.6 { "detected".into() } else { "—".into() }, cx))
                    .child(stat("Auto-gain", format!("{:+.1} dB", m.agc_gain_db), cx))
                    .child(stat("Compression", format!("{:.1} dB", m.comp_gr_db), cx))
                    .child(stat("Limiter", format!("{:.1} dB", m.limiter_gr_db), cx))
                    .child(stat("Room noise", format!("{:.0} dBFS", m.noise_floor_db), cx))
                    .child(stat("Gate", format!("{:.0} dB", m.exp_gain_db), cx)),
            )
    }

    fn render_tips(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        self.tips()
            .into_iter()
            .map(|t| {
                let key = t.key;
                Alert::new(SharedString::from(format!("tip-{key}")), t.message)
                    .with_variant(t.variant)
                    .title(t.title)
                    .on_close(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.dismissed.insert(key);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_recorder(&self, cx: &Context<Self>) -> impl IntoElement {
        let row = h_flex().gap_2().items_center();
        let row = if let Some(until) = self.recording_until {
            let left = until.saturating_duration_since(Instant::now()).as_secs_f32();
            row.child(div().text_color(cx.theme().danger).child(format!("Recording… {left:.0}s, talk normally")))
                .child(Button::new("rec-stop").small().label("Stop").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.finish_recording();
                    cx.notify();
                })))
        } else {
            row.child(
                Button::new("rec")
                    .label(format!("Record {RECORD_SECONDS}s A/B sample"))
                    .tooltip("Saves raw and enhanced versions side by side so you can compare them")
                    .disabled(self.engine.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.start_recording();
                        cx.notify();
                    })),
            )
        };
        card(cx).child(row.when_some(self.last_saved.clone(), |row, dir| {
            row.child(Button::new("open-rec").ghost().label("Open recordings").on_click(move |_, _, _| {
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }))
        }))
    }

    fn render_advanced(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let header = h_flex().justify_between().items_center().child(
            Button::new("advanced")
                .ghost()
                .label(if self.show_advanced { "▾ Advanced settings" } else { "▸ Advanced settings" })
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.show_advanced = !this.show_advanced;
                    cx.notify();
                })),
        );
        if !self.show_advanced {
            return card(cx).child(header);
        }

        let page_ix = self.settings_page;
        let page = &PAGES[page_ix];
        let header = header.child(
            Button::new("reset-page")
                .small()
                .ghost()
                .label(format!("Reset {}", page.name.to_lowercase()))
                .tooltip("Restore this page to the selected preset's values")
                .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| this.reset_settings_page(page_ix, w, cx))),
        );

        let tabs = TabBar::new("settings-tabs")
            .segmented()
            .selected_index(page_ix)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.settings_page = *ix;
                cx.notify();
            }))
            .children(PAGES.iter().map(|p| Tab::new().label(p.name)));

        let mut slider_ix = page_offset(page_ix);
        let mut groups = v_flex().gap_3();
        for (gi, group) in page.groups.iter().enumerate() {
            let on = group.toggle.as_ref().is_none_or(|t| (t.get)(&self.params) != 0);
            let title = v_flex()
                .gap_0p5()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(group.title))
                .child(div().text_xs().text_color(theme.muted_foreground).child(group.hint));
            let mut head = h_flex().justify_between().items_start().gap_3().child(title);
            if let Some(set) = group.toggle.as_ref().map(|t| t.set) {
                head = head.child(
                    Switch::new(SharedString::from(format!("group-{page_ix}-{gi}")))
                        .checked(on)
                        .tooltip(if on { "Turn off" } else { "Turn on" })
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            set(&mut this.params, *checked as u32);
                            this.preset = None;
                            this.push_params();
                            cx.notify();
                        })),
                );
            }
            let mut body = v_flex().gap_3().when(!on, |b| b.opacity(0.45));
            for knob in group.knobs {
                let state = &self.sliders[slider_ix];
                slider_ix += 1;
                if !(knob.visible)(&self.params) {
                    continue;
                }
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_sm().child(knob.label))
                                .child(div().text_sm().text_color(theme.primary).child(knob.fmt.format((knob.get)(&self.params)))),
                        )
                        .child(Slider::new(state).disabled(!on))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(knob.hint)),
                );
            }
            groups = groups.child(
                v_flex()
                    .gap_3()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(head)
                    .child(body),
            );
        }

        card(cx)
            .child(header)
            .child(tabs)
            .child(div().text_sm().text_color(theme.muted_foreground).child(page.blurb))
            .child(groups)
    }
}

fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex().gap_3().p_4().rounded_lg().border_1().border_color(theme.border).bg(theme.secondary)
}

fn labeled(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    h_flex().gap_3().items_center().child(div().w(px(90.)).text_sm().child(label)).child(div().flex_1().child(control))
}

fn stat(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(label))
        .child(div().text_sm().font_family("monospace").child(value))
}

fn meter(label: &'static str, db: f32, hold: Option<f32>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let color = if db > -1.0 {
        theme.danger
    } else if db > -12.0 {
        theme.warning
    } else {
        theme.success
    };
    let fmt = |v: f32| if v < -90.0 { "silent".to_string() } else { format!("{v:.1} dBFS") };
    let text = match hold {
        Some(h) => format!("{}  (peak {})", fmt(db), fmt(h)),
        None => fmt(db),
    };
    v_flex()
        .gap_1()
        .child(h_flex().justify_between().text_sm().child(label).child(div().text_color(theme.muted_foreground).child(text)))
        .child(
            div()
                .relative()
                .h(px(10.))
                .w_full()
                .rounded_full()
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .child(div().absolute().top_0().left_0().h_full().w(relative(frac)).rounded_full().bg(color))
                .when_some(hold, |el, h| {
                    let pos = ((h + 60.0) / 60.0).clamp(0.0, 1.0);
                    el.child(div().absolute().top_0().h_full().w(px(2.)).left(relative(pos)).bg(theme.foreground))
                }),
        )
}

// ---------------------------------------------------------------- headless self-test

/// Open the default devices, run the chain for 3 s with monitoring off, and print
/// what the meters saw. Exit code 0 = audio flowed through the chain.
fn selftest() -> i32 {
    let (din, dsend, dmonitor) = audio::default_devices();
    let (Some(input), Some(output)) = (din, dsend) else {
        println!("FAIL: no input/output device");
        return 1;
    };
    let shared = Shared::new(Params::far_mic());
    shared.monitor.store(false, Ordering::Relaxed);
    let engine = match Engine::start(&input, &output, dmonitor.as_deref(), shared.clone()) {
        Ok(e) => e,
        Err(e) => {
            println!("FAIL: {e:#}");
            return 1;
        }
    };
    println!("input:  {}\noutput: {}\nlatency: {:.1} ms", engine.input_desc, engine.output_desc, engine.chain_latency_ms);
    std::thread::sleep(Duration::from_secs(3));
    let m = *shared.meters.lock().unwrap();
    println!(
        "blocks {} · in peak {:.1} dBFS · out peak {:.1} dBFS · noise floor {:.1} dBFS · vad {:.2}",
        m.blocks, m.in_peak_db, m.out_peak_db, m.noise_floor_db, m.vad
    );
    if m.blocks > 100 {
        println!("PASS");
        0
    } else {
        println!("FAIL: chain did not receive audio");
        1
    }
}
