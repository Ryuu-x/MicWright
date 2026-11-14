//! The window: Dashboard, Sound, Presets, Soundboard, Devices and Settings pages, the
//! first-run setup and the hotkey picker. It draws [`Core`] and calls into it.

use crate::core::{self, Core, Kind, RECORD_SECONDS};
use crate::hotkey::Accel;
use crate::platform::{self, KeyState};
use crate::style::{card, card_label, card_value, color, icon, kind_color, muted, page_title, pill, section, settings_card, settings_row, tile, tile_title};
use crate::style::{ACCENT, BACKGROUND, CARD, MUTED, NAV_HOVER, NAV_SELECTED};
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use mw_engine::control::{Preset, Severity};
use mw_engine::settings::{self, PAGES, SIMPLE_KNOBS};
use std::collections::HashMap;

pub const MONITOR_OFF: &str = "Off";
const SIMPLE_BLURB: &str = "The settings that make the most difference. Turn on Advanced settings for the rest.";

actions!(micwright, [ToggleAb, ToggleMute]);

pub fn init(cx: &mut App) {
    // Only while the main view has focus: text boxes in dialogs keep their Space and M.
    cx.bind_keys([KeyBinding::new("space", ToggleAb, Some("MicWright")), KeyBinding::new("m", ToggleMute, Some("MicWright"))]);
}

/// The window's handle while one is open.
struct OpenWindow(Option<AnyWindowHandle>);
impl Global for OpenWindow {}

/// Brings the window up, opening it if it was closed to the tray.
pub fn show_window(core: &Entity<Core>, cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenWindow>().and_then(|w| w.0) {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("MicWright".into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(980.), px(720.)), cx))),
        window_min_size: Some(size(px(760.), px(560.))),
        app_id: Some(platform_app_id().into()),
        ..Default::default()
    };
    let core = core.clone();
    match gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| MainView::new(core.clone(), window, cx))) {
        Ok((handle, _)) => {
            cx.set_global(OpenWindow(Some(handle)));
            core.update(cx, |c, _| c.window_open = true);
        }
        Err(e) => eprintln!("couldn't open the window: {e:#}"),
    }
}

pub fn window_closed(core: &Entity<Core>, cx: &mut App) {
    cx.set_global(OpenWindow(None));
    core.update(cx, |c, _| c.window_open = false);
}

fn platform_app_id() -> &'static str {
    #[cfg(target_os = "linux")]
    return platform::APP_ID;
    #[cfg(not(target_os = "linux"))]
    return "MicWright";
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Sound,
    Presets,
    Soundboard,
    Devices,
    Settings,
}

const NAV: [(Page, &str, IconName); 6] = [
    (Page::Dashboard, "Dashboard", IconName::House),
    (Page::Sound, "Sound", IconName::SlidersHorizontal),
    (Page::Presets, "Presets", IconName::Star),
    (Page::Soundboard, "Soundboard", IconName::Music),
    (Page::Devices, "Devices", IconName::Mic),
    (Page::Settings, "Settings", IconName::Settings),
];

type DeviceSelect = SelectState<Vec<SharedString>>;

pub struct MainView {
    core: Entity<Core>,
    page: Page,
    settings_page: usize,
    sliders: HashMap<&'static str, Entity<SliderState>>,
    board_send: Entity<SliderState>,
    board_monitor: Entity<SliderState>,
    input_select: Entity<DeviceSelect>,
    send_select: Entity<DeviceSelect>,
    monitor_select: Entity<DeviceSelect>,
    seen_devices: u64,
    seen_params: u64,
    ticks: u64,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl MainView {
    fn new(core: Entity<Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::style::keep(cx);
        let mut subs = vec![
            cx.observe_in(&core, window, |this, _, window, cx| this.sync(window, cx)),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| crate::style::keep(cx)),
        ];
        let c = core.read(cx);
        let params = *c.ctl.params();
        let (send_level, monitor_level) = (c.prefs.board_volume, c.prefs.board_monitor_volume);

        let mut sliders = HashMap::new();
        for knob in settings::all_knobs() {
            let state = cx.new(|_| SliderState::new().min(knob.min).max(knob.max).step(knob.step).default_value((knob.get)(&params)));
            let key = knob.key;
            subs.push(cx.subscribe(&state, move |this, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(v) = ev else { return };
                let v = v.start();
                // Moving a slider back into sync with the engine is not an edit.
                this.core.update(cx, |c, cx| {
                    if c.ctl.param(key).is_some_and(|now| (now - v).abs() > 1e-6) {
                        c.set_param(key, v);
                        cx.notify();
                    }
                });
            }));
            sliders.insert(key, state);
        }
        let level = |v: f32, cx: &mut Context<Self>| cx.new(|_| SliderState::new().min(0.).max(100.).step(1.).default_value(v * 100.));
        let board_send = level(send_level, cx);
        let board_monitor = level(monitor_level, cx);
        for s in [&board_send, &board_monitor] {
            subs.push(cx.subscribe(s, |this, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(_) = ev else { return };
                let send = this.board_send.read(cx).value().start() / 100.;
                let monitor = this.board_monitor.read(cx).value().start() / 100.;
                this.core.update(cx, |c, cx| {
                    c.set_board_levels(send, monitor);
                    cx.notify();
                });
            }));
        }

        let input_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let send_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let monitor_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        subs.push(cx.subscribe_in(&input_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                this.core.update(cx, |c, cx| {
                    c.pick_input(name.to_string());
                    cx.notify();
                });
            }
        }));
        subs.push(cx.subscribe_in(&send_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                this.core.update(cx, |c, cx| {
                    c.pick_send(name.to_string());
                    cx.notify();
                });
            }
        }));
        subs.push(cx.subscribe_in(&monitor_select, window, |this, _, ev: &SelectEvent<Vec<SharedString>>, _, cx| {
            if let SelectEvent::Confirm(Some(name)) = ev {
                let pick = (name.as_ref() != MONITOR_OFF).then(|| name.to_string());
                this.core.update(cx, |c, cx| {
                    c.pick_monitor(pick);
                    cx.notify();
                });
            }
        }));

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut view = MainView {
            core,
            page: Page::Dashboard,
            settings_page: 0,
            sliders,
            board_send,
            board_monitor,
            input_select,
            send_select,
            monitor_select,
            seen_devices: u64::MAX,
            seen_params: 0,
            ticks: 0,
            focus,
            _subs: subs,
        };
        view.sync(window, cx);
        if !crate::prefs::Prefs::setup_done() || std::env::args().any(|a| a == "--setup") {
            let core = view.core.clone();
            window.defer(cx, move |window, cx| open_setup(core, window, cx));
        }
        view
    }

    /// Follows changes made outside the controls: device lists, presets, resets.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let c = self.core.read(cx);
        if c.devices_gen != self.seen_devices {
            self.seen_devices = c.devices_gen;
            let names = |list: &[String]| list.iter().map(|s| SharedString::from(s.clone())).collect::<Vec<_>>();
            let inputs = names(&c.inputs);
            let outputs = names(&c.outputs);
            // The virtual mic's own input is no place to listen.
            let monitors: Vec<SharedString> =
                std::iter::once(SharedString::from(MONITOR_OFF)).chain(outputs.iter().filter(|n| !mw_engine::audio::is_send_device(n)).cloned()).collect();
            let picks = (c.input.clone(), c.send.clone(), c.monitor.clone().unwrap_or_else(|| MONITOR_OFF.into()));
            for (select, items, pick) in [
                (&self.input_select, inputs, picks.0),
                (&self.send_select, outputs, picks.1),
                (&self.monitor_select, monitors, Some(picks.2)),
            ] {
                select.update(cx, |s, cx| {
                    let index = pick.as_ref().and_then(|p| items.iter().position(|i| i.as_ref() == p.as_str()));
                    s.set_items(items, window, cx);
                    s.set_selected_index(index.map(|i| IndexPath::default().row(i)), window, cx);
                });
            }
        }
        let c = self.core.read(cx);
        if c.params_gen != self.seen_params {
            self.seen_params = c.params_gen;
            let params = *c.ctl.params();
            for knob in settings::all_knobs() {
                if let Some(state) = self.sliders.get(knob.key) {
                    state.update(cx, |s, cx| s.set_value((knob.get)(&params), window, cx));
                }
            }
        }
        // Meters move 30×/s on the Dashboard and Soundboard; elsewhere ~5×/s is plenty, and
        // drawing less leaves the CPU to the audio.
        self.ticks += 1;
        if matches!(self.page, Page::Dashboard | Page::Soundboard) || self.ticks % 6 == 0 {
            cx.notify();
        }
    }

    /// Changes the app state, then redraws at once (not on the next slow tick).
    fn core<R>(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Core) -> R) -> R {
        let r = self.core.update(cx, |c, cx| {
            let r = f(c);
            cx.notify();
            r
        });
        cx.notify();
        r
    }
}

impl Render for MainView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match self.page {
            Page::Dashboard => self.dashboard(cx).into_any_element(),
            Page::Sound => self.sound(cx).into_any_element(),
            Page::Presets => self.presets(cx).into_any_element(),
            Page::Soundboard => self.soundboard(cx).into_any_element(),
            Page::Devices => self.devices(cx).into_any_element(),
            Page::Settings => self.app_settings(cx).into_any_element(),
        };
        let (title_bar, nav, toasts) = (self.title_bar(cx).into_any_element(), self.nav(cx).into_any_element(), self.toasts(cx).into_any_element());
        v_flex()
            .id("root")
            .key_context("MicWright")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleAb, _, cx| this.core(cx, |c| c.toggle_ab())))
            .on_action(cx.listener(|this, _: &ToggleMute, _, cx| this.core(cx, |c| c.toggle_mute())))
            .size_full()
            .bg(color(BACKGROUND))
            .text_color(color(0xFFFFFF))
            .text_size(px(14.))
            .child(title_bar)
            .child(
                h_flex().flex_1().min_h_0().items_start().child(nav).child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(
                            div()
                                .id("page")
                                .size_full()
                                .overflow_y_scroll()
                                .child(div().w_full().max_w(px(760.)).mx_auto().pt(px(4.)).pl(px(20.)).pr(px(28.)).pb(px(28.)).child(page)),
                        )
                        .child(toasts),
                ),
            )
    }
}

/// A row of fixed-width items that wraps, like WinUI's VariableSizedWrapGrid.
fn stat(label: &'static str, value: String) -> Div {
    h_flex().w(px(170.)).h(px(24.)).gap(px(8.)).items_center().child(muted(label)).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(value))
}

/// An icon-only button the height of a soundboard row.
fn row_button(id: impl Into<ElementId>, name: IconName, tip: &'static str) -> Button {
    Button::new(id).icon(Icon::new(name)).tooltip(tip).w(px(44.)).h(px(64.))
}

impl MainView {
    /// Brand on the left, what the engine is doing on the right.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, text, kind) = self.core.read(cx).status_line();
        h_flex()
            .h(px(48.))
            .flex_none()
            .px(px(16.))
            .justify_between()
            .items_center()
            .child(
                h_flex()
                    .gap(px(10.))
                    .items_center()
                    .child(icon(IconName::SquareActivity, 16., color(ACCENT)))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child("MICWRIGHT")),
            )
            .child(pill(text, kind))
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(px(176.)).flex_none().px(px(4.)).gap(px(4.)).children(NAV.iter().map(|&(page, label, name)| {
            let selected = self.page == page;
            h_flex()
                .id(SharedString::from(format!("nav-{label}")))
                .relative()
                .h(px(36.))
                .px(px(12.))
                .gap(px(14.))
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .map(|el| if selected { el.bg(color(NAV_SELECTED)) } else { el.hover(|s| s.bg(color(NAV_HOVER))) })
                .child(icon(name, 16., color(0xFFFFFF)))
                .child(div().text_size(px(14.)).child(label))
                // WinUI's selection indicator.
                .when(selected, |el| el.child(div().absolute().left(px(0.)).top(px(10.)).w(px(3.)).h(px(16.)).rounded(px(2.)).bg(color(ACCENT))))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.page = page;
                    cx.notify();
                }))
        }))
    }

    /// Live warnings float over the content, bottom right, so they never push the layout around.
    fn toasts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let alerts = self.core.read(cx).visible_alerts();
        let tips_on = self.core.read(cx).prefs.show_tips;
        v_flex().absolute().bottom(px(24.)).right(px(24.)).w(px(400.)).gap(px(8.)).children(alerts.into_iter().map(|a| {
            let key = a.key.clone();
            let is_tip = !matches!(key.as_str(), "error" | "virtual-mic");
            let alert = match a.severity {
                Severity::Error => Alert::error(SharedString::from(format!("toast-{key}")), a.message.clone()),
                Severity::Warning => Alert::warning(SharedString::from(format!("toast-{key}")), a.message.clone()),
                Severity::Info => Alert::info(SharedString::from(format!("toast-{key}")), a.message.clone()),
            };
            // Opaque backing: the alert's own tint is translucent.
            v_flex()
                .rounded(px(8.))
                .bg(color(0x202022))
                .border_1()
                .border_color(color(0x3A3A3D))
                .shadow_lg()
                .child(alert.title(a.title.clone()).on_close(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let key = key.clone();
                    this.core(cx, |c| {
                        c.dismissed.insert(key);
                    })
                })))
                .when(is_tip && tips_on, |el| {
                    el.child(
                        h_flex().px(px(12.)).pb(px(8.)).child(
                            Button::new(SharedString::from(format!("tips-off-{}", a.key)))
                                .link()
                                .xsmall()
                                .label("Turn off tips")
                                .tooltip("Turn them back on under Settings > Pop-up tips")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.set_show_tips(false)))),
                        ),
                    )
                })
        }))
    }

    // ------------------------------------------------------------------ dashboard

    /// The path your voice takes, top to bottom.
    fn dashboard(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let (status, _, _) = c.status_line();
        let running = c.ctl.status().running;
        let (verdict, vkind) = c.mic_verdict();
        let nf = c.meters.noise_floor_db;
        let room = if nf < -100.0 {
            ""
        } else if nf > -45.0 {
            "Room: noisy"
        } else if nf > -55.0 {
            "Room: some noise"
        } else {
            "Room: quiet"
        };
        let is_muted = c.muted();
        let enhanced = c.enhanced();
        let (ns_on, eq_on) = (c.on("ns_enabled"), c.on("eq_enabled"));
        let preset = c.preset_label();
        let input = c.input.clone().unwrap_or_else(|| "None".into());
        let output = if c.send_is_virtual() { "Virtual Microphone".to_string() } else { c.send.clone().unwrap_or_else(|| "None".into()) };
        let (out_pill, out_kind) = if !running {
            ("Not sending", Kind::Bad)
        } else if is_muted {
            ("Muted", Kind::Warn)
        } else {
            ("Sending", Kind::Good)
        };
        let wave: Vec<f32> = c.wave.iter().copied().collect();

        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap(px(16.))
                    .child(v_flex().gap(px(2.)).min_w_0().child(page_title("Audio Engine")).child(muted(status)))
                    .child(
                        Button::new("start-stop")
                            .label(if running { "Stop" } else { "Start" })
                            .min_w(px(72.))
                            .when(!running, |b| b.primary())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.toggle_engine()))),
                    ),
            )
            .child(
                card()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_start()
                            .gap(px(16.))
                            .child(v_flex().gap(px(6.)).min_w_0().child(card_label("INPUT DEVICE")).child(card_value(input)))
                            .child(
                                Button::new("mute")
                                    .icon(Icon::new(if is_muted { IconName::MicOff } else { IconName::Mic }))
                                    .label(if is_muted { "Muted" } else { "Mute" })
                                    .when(is_muted, |b| b.primary())
                                    .tooltip("Mute the output (M)")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.toggle_mute()))),
                            ),
                    )
                    .child(h_flex().h(px(32.)).items_center().children(wave.into_iter().map(|f| {
                        div().flex_1().flex().justify_center().child(div().w(px(3.)).h(px(3. + f * 29.)).rounded(px(1.5)).bg(color(ACCENT)))
                    })))
                    .child(h_flex().gap(px(12.)).items_center().child(pill(verdict, vkind)).child(muted(room))),
            )
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(self.toggle_tile("tile-ab", IconName::Zap, "Enhancement", enhanced, "Compare raw and enhanced (Space)", cx, |c| c.toggle_ab()))
                    .child(self.toggle_tile("tile-ns", IconName::SlidersHorizontal, "Noise Removal", ns_on, "Removes fans, PC hum and room noise", cx, |c| {
                        c.toggle_param("ns_enabled")
                    }))
                    .child(self.toggle_tile("tile-eq", IconName::ChartArea, "Equalizer", eq_on, "Bass, body and brightness", cx, |c| c.toggle_param("eq_enabled"))),
            )
            .child(
                tile("tile-preset")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.page = Page::Presets;
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .gap(px(12.))
                            .child(v_flex().gap(px(6.)).min_w_0().child(card_label("PRESET")).child(card_value(preset)))
                            .child(icon(IconName::ChevronRight, 14., color(MUTED))),
                    ),
            )
            .child(card().gap(px(6.)).child(card_label("OUTPUT DEVICE")).child(card_value(output)).child(h_flex().mt(px(2.)).child(pill(out_pill, out_kind))))
            .child(muted("Space compares raw and enhanced  ·  M mutes").text_size(px(12.)).mt(px(2.)).ml(px(1.)))
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_tile(&self, id: &'static str, name: IconName, title: &'static str, on: bool, tip: &'static str, cx: &mut Context<Self>, toggle: fn(&mut Core)) -> impl IntoElement {
        tile(id)
            .flex_1()
            .gap(px(8.))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.core(cx, toggle)))
            .child(icon(name, 16., color(0xFFFFFF)))
            .child(tile_title(title))
            .child(h_flex().child(pill(if on { "ON" } else { "OFF" }, if on { Kind::Good } else { Kind::Off })))
    }

    fn stats(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let m = c.meters;
        let s = c.ctl.status();
        let level = |db: f32| if db < -90.0 { "silent".to_string() } else { format!("{db:.1} dBFS") };
        let reduction = |db: f32| if db.abs() < 0.05 { "none".to_string() } else { format!("{:.1} dB", -db.abs()) };
        h_flex()
            .flex_wrap()
            .child(stat("Voice", if c.talking() { "Talking".into() } else { "Not talking".into() }))
            .child(stat("Mic peak", level(c.hold)))
            .child(stat("Output peak", level(m.out_peak_db)))
            .child(stat("Room noise", if m.noise_floor_db < -100.0 { "—".into() } else { format!("{:.0} dBFS", m.noise_floor_db) }))
            .child(stat("Auto level", format!("{:+.1} dB", m.agc_gain_db)))
            .child(stat("Smoothing", reduction(m.comp_gr_db)))
            .child(stat("Limiter", reduction(m.limiter_gr_db)))
            .child(stat("Gate", reduction(m.exp_gain_db)))
            .child(stat("Latency", if s.running { format!("≈ {:.0} ms", s.latency_ms) } else { "—".into() }))
            .child(stat("Dropouts", if s.running { s.dropouts.to_string() } else { "—".into() }))
    }

    // ------------------------------------------------------------------ sound

    /// Generated from the engine's settings table.
    fn sound(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let show_all = c.prefs.show_all_settings;
        let params = *c.ctl.params();
        let page_ix = self.settings_page;
        let reset_label = if c.undo.is_some() { "Undo reset".to_string() } else { format!("Reset {}", PAGES[page_ix].name.to_lowercase()) };
        let agc_off = params.agc_enabled == 0;
        let groups: Vec<&settings::Group> =
            if show_all { PAGES[page_ix].groups.iter().collect() } else { PAGES.iter().flat_map(|p| p.groups.iter()).filter(|g| g.knobs.iter().any(|k| SIMPLE_KNOBS.contains(&k.key))).collect() };

        let mut list = v_flex().gap(px(8.));
        for group in groups {
            let on = group.toggle.as_ref().is_none_or(|t| (t.get)(&params) != 0);
            let switch = group.toggle.as_ref().map(|toggle| {
                let key = toggle.key;
                Switch::new(SharedString::from(format!("group-{key}"))).checked(on).label(if on { "On" } else { "Off" }).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.core(cx, |c| {
                        c.set_param(key, if *checked { 1.0 } else { 0.0 });
                        c.params_gen += 1;
                    })
                }))
            });
            // WinUI's SettingsExpander: the group's header, then one row per setting.
            let mut expander = v_flex()
                .bg(color(CARD))
                .rounded(px(12.))
                .child(settings_row(None, group.title, group.hint, div().children(switch)).px(px(16.)).py(px(14.)));
            for knob in group.knobs.iter().filter(|k| show_all || SIMPLE_KNOBS.contains(&k.key)) {
                if !(knob.visible)(&params) || (settings::visible_when_agc_off(knob.key) && !agc_off) {
                    continue;
                }
                let Some(state) = self.sliders.get(knob.key) else { continue };
                expander = expander.child(
                    settings_row(
                        None,
                        knob.label,
                        knob.hint,
                        h_flex()
                            .gap(px(12.))
                            .items_center()
                            .child(div().min_w(px(72.)).text_right().text_color(color(0xB4A9FA)).child(knob.fmt.format((knob.get)(&params))))
                            .child(div().w(px(240.)).child(Slider::new(state).disabled(!on))),
                    )
                    .pl(px(48.))
                    .pr(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(color(0x1E1E20))
                    .when(!on, |r| r.opacity(0.45)),
                );
            }
            list = list.child(expander);
        }

        let c = self.core.read(cx);
        let recording = c.ctl.recording_left();
        let has_latest = c.latest.is_some();
        let playing = c.playing_recording();
        let record_note = match (recording, &c.play_error) {
            (Some(_), _) => "Recording… talk normally.".to_string(),
            (None, Some(e)) => e.clone(),
            _ => format!("Records {RECORD_SECONDS} seconds and saves a raw and an enhanced copy."),
        };
        let can_record = c.ctl.status().running || recording.is_some();

        v_flex()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .mb(px(8.))
                    .child(page_title("Sound"))
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .items_center()
                            .when(show_all, |el| {
                                el.child(
                                    Button::new("reset-page")
                                        .link()
                                        .label(reset_label)
                                        .tooltip("Back to the values from your current preset")
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.core(cx, |c| c.reset_or_undo(page_ix)))),
                                )
                            })
                            .child(
                                Checkbox::new("show-all")
                                    .label("Advanced settings")
                                    .checked(show_all)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| this.core(cx, |c| c.set_show_all(*checked)))),
                            ),
                    ),
            )
            .when(show_all, |el| {
                el.child(
                    div().mb(px(8.)).child(
                        TabBar::new("pages")
                            .underline()
                            .selected_index(page_ix)
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.settings_page = *ix;
                                this.core(cx, |c| c.undo = None);
                            }))
                            .children(PAGES.iter().map(|p| Tab::new().label(p.name))),
                    ),
                )
            })
            .child(muted(if show_all { PAGES[page_ix].blurb } else { SIMPLE_BLURB }).ml(px(1.)).mb(px(12.)))
            .child(list)
            .child(section("COMPARE").mb(px(8.)))
            .child(settings_card(
                Some(IconName::Disc),
                "Record an A/B sample",
                record_note,
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .when(has_latest && recording.is_none(), |el| {
                        el.child(Button::new("open-rec").link().label("Open folder").on_click(|_, _, _| {
                            let dir = mw_engine::control::recordings_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            platform::open_path(&dir);
                        }))
                        .child(
                            Button::new("play-raw")
                                .label(if playing == Some(false) { "Stop" } else { "Play raw" })
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.play_recording(false)))),
                        )
                        .child(
                            Button::new("play-enh")
                                .label(if playing == Some(true) { "Stop" } else { "Play enhanced" })
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.play_recording(true)))),
                        )
                    })
                    .child(
                        Button::new("record")
                            .min_w(px(96.))
                            .label(match recording {
                                Some(left) => format!("Stop ({left:.0}s)"),
                                None => "Record".into(),
                            })
                            .disabled(!can_record)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.toggle_recording()))),
                    ),
            ))
    }

    // ------------------------------------------------------------------ presets

    fn presets(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let current = c.ctl.current_key();
        let picked = c.ctl.picked_key();
        let customs = c.ctl.custom_presets();
        let edited = current.is_empty();
        let edited_name = c.preset_label();
        let can_revert = c.preset_name(&picked).is_some();

        let preset_card = |key: String, name: String, tip: Option<&'static str>, cx: &mut Context<Self>| {
            let state = if key == current {
                Some(("Active", Kind::Good))
            } else if current.is_empty() && key == picked {
                Some(("Edited", Kind::Warn))
            } else {
                None
            };
            let k = key.clone();
            tile(SharedString::from(format!("preset-{key}")))
                .flex_1()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.core(cx, |c| c.apply_preset(&k))))
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(px(12.))
                        .child(v_flex().gap(px(2.)).min_w_0().child(tile_title(name)).when_some(tip, |el, t| el.child(muted(t))))
                        .when_some(state, |el, (text, kind)| el.child(pill(text, kind))),
                )
        };

        let mut builtin = v_flex().gap(px(8.));
        for (_, key, name, tip) in Preset::ALL {
            builtin = builtin.child(preset_card(key.to_string(), name.to_string(), Some(tip), cx));
        }
        let mut mine = v_flex().gap(px(8.));
        for name in &customs {
            let n = name.clone();
            mine = mine.child(
                h_flex().gap(px(8.)).child(preset_card(format!("custom:{name}"), name.clone(), None, cx)).child(
                    Button::new(SharedString::from(format!("delete-preset-{name}")))
                        .icon(Icon::new(IconName::Trash))
                        .tooltip("Delete")
                        .w(px(44.))
                        .h(px(48.))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let core = this.core.clone();
                            let name = n.clone();
                            window.open_alert_dialog(cx, move |alert, _, _| {
                                let core = core.clone();
                                let name = name.clone();
                                alert
                                    .title(format!("Delete “{name}”?"))
                                    .description("This removes the saved preset. Your current sound stays as it is.")
                                    .ok_text("Delete")
                                    .show_cancel(true)
                                    .on_ok(move |_, _, cx| {
                                        core.update(cx, |c, cx| {
                                            c.delete_preset(&name);
                                            cx.notify();
                                        });
                                        true
                                    })
                            });
                        })),
                ),
            );
        }

        v_flex()
            .gap(px(8.))
            .child(
                h_flex().justify_between().items_center().child(page_title("Presets")).child(Button::new("save-preset").label("Save current sound…").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        let core = this.core.clone();
                        open_save_preset(core, window, cx);
                    },
                ))),
            )
            .when(edited, |el| {
                el.child(h_flex().gap(px(4.)).items_center().child(muted(edited_name).ml(px(1.)).mr(px(4.))).when(can_revert, |el| {
                    el.child(Button::new("revert").link().label("Revert").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.revert()))))
                }))
            })
            .child(builtin)
            .child(muted("Far mic, Quiet room and Noisy room keep your Tone and Effects settings.").mt(px(4.)).ml(px(1.)))
            .child(section("MY PRESETS").mt(px(12.)))
            .when(customs.is_empty(), |el| el.child(muted("Nothing saved yet. Tune your sound, then choose Save current sound.").ml(px(1.))))
            .child(mine)
    }

    // ------------------------------------------------------------------ soundboard

    fn soundboard(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let clips: Vec<(String, String, f32)> = c.ctl.board_clips().iter().map(|clip| (clip.id.clone(), clip.name.clone(), clip.seconds())).collect();
        let playing = c.playing();
        let status = c.board_status();
        let problem = c.board_problem.clone().or_else(|| if c.prefs.board_hotkeys { c.hotkeys.problem() } else { None });
        let (playing_text, playing_kind) = c.playing_pill();
        let hotkeys_on = c.prefs.board_hotkeys;
        let stop_on = c.prefs.board_stop_key_on;
        let stop_key = Accel::parse(&c.prefs.board_stop_key).map(|a| a.label()).unwrap_or_default();
        let stop_state = c.key_state("stop").cloned();
        let keys: HashMap<String, (String, Option<KeyState>)> = c
            .prefs
            .board_keys
            .iter()
            .filter_map(|(id, k)| Some((id.clone(), (Accel::parse(k)?.label(), c.key_state(id).cloned()))))
            .collect();
        let kind = c.hotkeys.kind();
        let busy = c.board_busy();
        let send_level = c.prefs.board_volume;
        let monitor_level = c.prefs.board_monitor_volume;

        let key_pill = |label: &str, state: Option<&KeyState>| match (hotkeys_on, state) {
            (false, _) => pill(label.to_string(), Kind::Off),
            (true, Some(KeyState::Active(t))) => pill(t.clone(), Kind::Good),
            (true, Some(KeyState::Taken)) => pill(format!("{label} in use"), Kind::Warn),
            (true, Some(KeyState::NotAssigned)) => pill(format!("{label} not set"), Kind::Warn),
            (true, None) => pill(label.to_string(), Kind::Off),
        };

        let mut list = v_flex().gap(px(8.));
        for (id, name, seconds) in clips.clone() {
            let now = playing.iter().find(|p| p.0 == id);
            let key = keys.get(&id);
            let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
            let (n, n2) = (name.clone(), name.clone());
            list = list.child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        tile(SharedString::from(format!("clip-{id}")))
                            .flex_1()
                            .min_w_0()
                            .h(px(64.))
                            .py(px(10.))
                            .justify_center()
                            .gap(px(6.))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.core(cx, |c| c.toggle_clip(&i1, false))))
                            .child(
                                h_flex()
                                    .gap(px(14.))
                                    .items_center()
                                    .child(if now.is_some() { icon(IconName::Square, 14., color(ACCENT)) } else { icon(IconName::Play, 14., color(0xFFFFFF)) })
                                    .child(v_flex().flex_1().min_w_0().child(tile_title(name.clone()).truncate()).child(muted(format!("{seconds:.1} s"))))
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .when_some(now, |el, p| el.child(pill(if p.1 { "Preview" } else { "Playing" }, Kind::Good)))
                                            .when_some(key, |el, (label, state)| el.child(key_pill(label, state.as_ref()))),
                                    ),
                            )
                            // Hidden rather than removed, so rows don't jump when a sound starts.
                            .child(div().when(now.is_none(), |d| d.opacity(0.)).child(Progress::new(SharedString::from(format!("bar-{id}"))).value(now.map(|p| p.2 * 100.).unwrap_or(0.)))),
                    )
                    .child(row_button(SharedString::from(format!("preview-{id}")), IconName::Headphones, "Preview: only you hear it").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.core(cx, |c| c.toggle_clip(&i2, true)),
                    )))
                    .child(row_button(SharedString::from(format!("key-{id}")), IconName::Keyboard, "Set hotkey").on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            let core = this.core.clone();
                            open_hotkey_picker(core, HotkeyTarget::Clip(i3.clone(), n.clone()), window, cx);
                        },
                    )))
                    .child(row_button(SharedString::from(format!("del-{id}")), IconName::Trash, "Delete").on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            let core = this.core.clone();
                            let (id, name) = (i4.clone(), n2.clone());
                            window.open_alert_dialog(cx, move |alert, _, _| {
                                let (core, id) = (core.clone(), id.clone());
                                alert
                                    .title(format!("Delete “{name}”?"))
                                    .description("This removes the sound and its hotkey from the app. Your original file stays where it is.")
                                    .ok_text("Delete")
                                    .show_cancel(true)
                                    .on_ok(move |_, _, cx| {
                                        core.update(cx, |c, cx| {
                                            c.remove_clip(&id);
                                            cx.notify();
                                        });
                                        true
                                    })
                            });
                        },
                    ))),
            );
        }

        let hotkey_note = match kind {
            "portal" => "Play sounds from any app. Your desktop asks you once to allow new hotkeys.",
            "x11" => "Play sounds from any app, even with this window closed to the tray.",
            _ => "Global hotkeys are part of the Windows app.",
        };
        let first_name = self.core.read(cx).ctl.board_clips().first().map(|c| c.name.clone()).unwrap_or_else(|| "Airhorn".into());
        let level = |id: &'static str, state: &Entity<SliderState>, value: f32| {
            h_flex()
                .gap(px(12.))
                .items_center()
                .child(div().min_w(px(44.)).text_right().text_color(color(0xB4A9FA)).child(format!("{:.0}%", value * 100.)))
                .child(div().id(id).w(px(200.)).child(Slider::new(state)))
        };
        let stop_label = match (&stop_state, hotkeys_on && stop_on) {
            (Some(KeyState::Active(t)), true) => t.clone(),
            (Some(KeyState::Taken), true) => format!("{stop_key} (in use)"),
            _ => stop_key.clone(),
        };

        v_flex()
            .gap(px(8.))
            .child(
                h_flex().justify_between().items_center().child(page_title("Soundboard")).child(
                    h_flex()
                        .gap(px(8.))
                        .child(Button::new("stop-all").label("Stop all").disabled(playing.is_empty()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.stop_all_clips()))))
                        .child(Button::new("add-sounds").primary().label(if busy { "Adding…" } else { "Add sounds…" }).disabled(busy).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_sounds(cx)))),
                ),
            )
            .child(muted(status).ml(px(1.)))
            .when_some(problem, |el, p| el.child(div().text_size(px(13.)).text_color(kind_color(Kind::Bad)).ml(px(1.)).child(p)))
            .when(clips.is_empty(), |el| {
                el.child(muted("No sounds yet. Add MP3, WAV, OGG, FLAC or M4A files up to a minute long. The app keeps its own copy, levelled so one sound isn't louder than the rest.").mt(px(8.)).ml(px(1.)))
            })
            .child(list)
            .child(section("VOLUME").mt(px(12.)))
            .child(settings_card(Some(IconName::Volume2), "Sound volume", "How loud other people hear your sounds.", level("send-level", &self.board_send, send_level)))
            .child(settings_card(
                Some(IconName::Headphones),
                "In my headphones",
                "How loud you hear them on the Monitor device. Separate from Hear myself.",
                level("monitor-level", &self.board_monitor, monitor_level),
            ))
            .child(section("HOTKEYS").mt(px(12.)))
            .child(settings_card(
                Some(IconName::Keyboard),
                "Hotkeys",
                hotkey_note,
                Switch::new("hotkeys").checked(hotkeys_on).label(if hotkeys_on { "On" } else { "Off" }).on_click(cx.listener(|this, on: &bool, _, cx| this.core(cx, |c| c.set_hotkeys_on(*on)))),
            ))
            .child(
                tile("stop-card").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.core(cx, |c| c.stop_all_clips()))).child(settings_row(
                    Some(IconName::CircleStop),
                    "Stop all sounds",
                    "Click here, or use the hotkey, to cut off whatever is playing.",
                    h_flex()
                        .gap(px(12.))
                        .items_center()
                        .child(pill(playing_text, playing_kind))
                        .child(Button::new("stop-key").min_w(px(110.)).label(stop_label).tooltip("Change the hotkey").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let core = this.core.clone();
                            open_hotkey_picker(core, HotkeyTarget::Stop, window, cx);
                        })))
                        .child(Switch::new("stop-key-on").checked(stop_on).label(if stop_on { "On" } else { "Off" }).on_click(cx.listener(|this, on: &bool, _, cx| {
                            cx.stop_propagation();
                            this.core(cx, |c| c.set_stop_key_on(*on))
                        }))),
                )),
            )
            .child(
                muted(format!("On any desktop you can also bind  micwright --play \"{first_name}\",  micwright --stop-sounds  or  micwright --toggle-mute  to keys in your desktop's keyboard settings."))
                    .text_size(px(12.))
                    .mt(px(4.))
                    .ml(px(1.)),
            )
    }

    fn add_sounds(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
        let core = self.core.clone();
        cx.spawn(async move |_, cx| {
            let result = paths.await;
            core.update(cx, |c, cx| {
                match result {
                    Ok(Ok(Some(files))) => c.add_sounds(files),
                    // Desktops without a file picker portal (some minimal window managers).
                    Ok(Err(e)) => {
                        c.board_problem = Some(format!("Couldn't open the file picker ({e:#}). You can add sounds with: micwright --add-sound <file>"))
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------ devices

    fn devices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let virtual_send = c.send_is_virtual();
        let show_send = !virtual_send || c.prefs.show_all_settings;
        let default_mic = c.prefs.default_mic;
        let monitor_on = c.ctl.monitor();
        let dismissed = c.dismissed.contains("discord");
        let discord = if default_mic { "Leave Discord's Input Device on \"Default\"" } else { "Set Discord's Input Device to \"MicWright Virtual Mic\"" };

        v_flex()
            .gap(px(8.))
            .child(page_title("Devices"))
            .child(settings_card(Some(IconName::Mic), "Microphone", "The mic to enhance.", div().w(px(280.)).child(Select::new(&self.input_select).id("input-select"))))
            .when(show_send, |el| {
                el.child(settings_card(Some(IconName::Cable), "Send to", platform::SEND_HINT, div().w(px(280.)).child(Select::new(&self.send_select).id("send-select"))))
            })
            .child(settings_card(
                Some(IconName::Headphones),
                "Monitor",
                "Hear yourself on headphones while you talk.",
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .child(div().w(px(220.)).child(Select::new(&self.monitor_select).id("monitor-select")))
                    .child(Switch::new("hear-myself").checked(monitor_on).label(if monitor_on { "On" } else { "Off" }).on_click(cx.listener(|this, on: &bool, _, cx| this.core(cx, |c| c.set_monitor_on(*on))))),
            ))
            .when(!show_send, |el| {
                el.child(muted("Your voice reaches other apps through MicWright Virtual Mic. To send it somewhere else, turn on Advanced settings on the Sound page.").mt(px(4.)).ml(px(1.)))
            })
            .when(virtual_send && !dismissed, |el| {
                el.child(
                    div().mt(px(4.)).bg(color(CARD)).rounded(px(12.)).child(
                        Alert::info("discord", format!("{discord} and turn off Discord's own noise suppression, echo cancellation and automatic gain."))
                            .title("Using this with Discord")
                            .on_close(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.core(cx, |c| {
                                    c.dismissed.insert("discord".into());
                                })
                            })),
                    ),
                )
            })
    }

    // ------------------------------------------------------------------ settings

    fn app_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let default_mic = c.prefs.default_mic;
        let default_desc = match (&c.default_mic_error, default_mic) {
            (Some(e), true) => e.clone(),
            _ => "Makes the enhanced voice (MicWright Virtual Mic) your desktop's default mic, so apps set to \"Default\" use it. Apps set to a specific mic keep that mic. Turning it on also turns on Start at login, so they keep working after a restart. Turning it off, or quitting, puts your previous default mic back.".into(),
        };
        let startup = platform::start_with_system();
        let tips = c.prefs.show_tips;
        let tray_note = if c.has_tray { "" } else { " Your desktop shows no tray icons, so closing the window quits MicWright." };
        let config = mw_engine::control::config_dir();
        let stats = self.stats(cx);
        let on_off = |on: bool| if on { "On" } else { "Off" };

        v_flex()
            .gap(px(8.))
            .child(page_title("Settings"))
            .child(settings_card(
                Some(IconName::Mic),
                "Use as the default mic",
                format!("{default_desc}{tray_note}"),
                Switch::new("default-mic").checked(default_mic).label(on_off(default_mic)).on_click(cx.listener(|this, on: &bool, _, cx| this.core(cx, |c| c.set_default_mic(*on)))),
            ))
            .child(settings_card(
                Some(IconName::Power),
                "Start at login",
                "Starts in the tray when you log in, so your enhanced mic is always ready.",
                Switch::new("startup").checked(startup).label(on_off(startup)).on_click(cx.listener(|this, on: &bool, _, cx| {
                    if let Err(e) = platform::set_start_with_system(*on) {
                        this.core(cx, |c| c.board_problem = Some(e));
                    }
                    cx.notify();
                })),
            ))
            .child(settings_card(
                Some(IconName::Lightbulb),
                "Pop-up tips",
                "Hints in the corner about your mic level, clipping and room noise. Audio errors always show.",
                Switch::new("tips").checked(tips).label(on_off(tips)).on_click(cx.listener(|this, on: &bool, _, cx| this.core(cx, |c| c.set_show_tips(*on)))),
            ))
            .child(settings_card(
                Some(IconName::WandSparkles),
                "Setup",
                "Walk through choosing your mic, checking your level and picking where your voice goes.",
                Button::new("run-setup").label("Run setup again").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let core = this.core.clone();
                    open_setup(core, window, cx);
                })),
            ))
            .child(section("DETAILS").mt(px(12.)))
            .child(card().child(stats))
            .child(muted(format!("MicWright {} · settings in {}", env!("CARGO_PKG_VERSION"), config.display())).text_size(px(12.)).mt(px(4.)).ml(px(1.)))
    }
}

// ---------------------------------------------------------------------- save preset

struct SavePreset {
    core: Entity<Core>,
    input: Entity<InputState>,
    error: Option<String>,
}

fn open_save_preset(core: Entity<Core>, window: &mut Window, cx: &mut App) {
    let current = core.read(cx).ctl.current_key();
    let name = current.strip_prefix("custom:").unwrap_or("").to_string();
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Streaming, Late night, Podcast").default_value(name));
    let view = cx.new(|_| SavePreset { core, input: input.clone(), error: None });
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
        // Enter in the name box confirms the dialog: save, and stay open on a bad name.
        let view = view.clone();
        dialog.title("Save as preset").w(px(440.)).child(view.clone()).on_ok(move |_, window, cx| view.update(cx, |v, cx| v.save(window, cx)))
    });
}

impl SavePreset {
    /// True when saved, so the dialog can close.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.input.read(cx).value().to_string();
        match self.core.update(cx, |c, cx| {
            cx.notify();
            c.save_preset(&name)
        }) {
            Ok(()) => {
                window.close_dialog(cx);
                true
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                false
            }
        }
    }
}

impl Render for SavePreset {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(muted("Saves every sound setting, so you can switch back to this sound in one click. A preset with the same name is replaced."))
            .child(Input::new(&self.input))
            .when_some(self.error.clone(), |el, e| el.child(div().text_sm().text_color(cx.theme().danger).child(e)))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("save").primary().label("Save").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.save(window, cx);
                    }))),
            )
    }
}

// ---------------------------------------------------------------------- hotkey picker

#[derive(Clone)]
enum HotkeyTarget {
    Stop,
    Clip(String, String),
}

struct HotkeyPicker {
    core: Entity<Core>,
    target: HotkeyTarget,
    picked: Option<Accel>,
    shown: String,
    note: String,
    focus: FocusHandle,
}

fn open_hotkey_picker(core: Entity<Core>, target: HotkeyTarget, window: &mut Window, cx: &mut App) {
    // A registered hotkey would play its sound instead of reaching the picker.
    core.update(cx, |c, _| {
        c.picking_hotkey = true;
        c.register_hotkeys();
    });
    let current = {
        let p = &core.read(cx).prefs;
        match &target {
            HotkeyTarget::Stop => Some(p.board_stop_key.clone()),
            HotkeyTarget::Clip(id, _) => p.board_keys.get(id).cloned(),
        }
    };
    let title = match &target {
        HotkeyTarget::Stop => "Hotkey to stop all sounds".to_string(),
        HotkeyTarget::Clip(_, name) => format!("Hotkey for “{name}”"),
    };
    let view = cx.new(|cx| HotkeyPicker {
        core: core.clone(),
        target,
        picked: None,
        shown: current.and_then(|k| Accel::parse(&k)).map(|a| a.label()).unwrap_or_default(),
        note: "Press the keys you want.".into(),
        focus: cx.focus_handle(),
    });
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
        }
        let core = core.clone();
        let picker = view.clone();
        dialog
            .title(title.clone())
            .w(px(440.))
            .child(view.clone())
            // Enter saves what was picked; with nothing usable picked it does nothing.
            .on_ok(move |_, window, cx| {
                picker.update(cx, |p, cx| {
                    if let Some(key) = p.picked.clone() {
                        p.finish(Some(Some(key)), window, cx);
                    }
                });
                false
            })
            .on_close(move |_, _, cx| {
                core.update(cx, |c, _| {
                    c.picking_hotkey = false;
                    c.register_hotkeys();
                });
            })
    });
}

impl HotkeyPicker {
    fn finish(&mut self, key: Option<Option<Accel>>, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.target.clone();
        let result = self.core.update(cx, |c, cx| {
            c.picking_hotkey = false;
            let r = match (key, &target) {
                (None, _) => {
                    c.register_hotkeys();
                    Ok(())
                }
                (Some(k), HotkeyTarget::Clip(id, _)) => c.set_clip_key(id, k),
                (Some(Some(k)), HotkeyTarget::Stop) => c.set_stop_key(k),
                (Some(None), HotkeyTarget::Stop) => Ok(()),
            };
            if r.is_err() {
                c.register_hotkeys();
            }
            cx.notify();
            r
        });
        if let Err(e) = result {
            self.core.update(cx, |c, _| c.board_problem = Some(e));
        }
        window.close_dialog(cx);
    }
}

impl Render for HotkeyPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let can_remove = matches!(self.target, HotkeyTarget::Clip(..)) && !self.shown.is_empty() && self.picked.is_none();
        v_flex()
            .gap_3()
            .child(
                div()
                    .id("hotkey-box")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        let k = &ev.keystroke;
                        let accel = Accel { ctrl: k.modifiers.control, alt: k.modifiers.alt, shift: k.modifiers.shift, key: k.key.clone() };
                        this.shown = accel.label();
                        if k.modifiers.platform {
                            this.note = "Shortcuts with the Super key belong to the desktop.".into();
                            this.picked = None;
                        } else if let Some(problem) = accel.problem() {
                            this.note = problem.into();
                            this.picked = None;
                        } else {
                            this.note = "Save to use it, or press something else.".into();
                            this.picked = Some(accel);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.primary)
                    .bg(theme.background)
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.shown.is_empty() { "Press a key combination".to_string() } else { self.shown.clone() }),
            )
            .child(muted(self.note.clone()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .when(can_remove, |el| el.child(Button::new("remove").label("Remove").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.finish(Some(None), window, cx)))))
                    .child(Button::new("cancel").label("Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.finish(None, window, cx))))
                    .child(Button::new("save").primary().label("Save").disabled(self.picked.is_none()).on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        let key = this.picked.clone();
                        this.finish(Some(key), window, cx)
                    }))),
            )
    }
}

// ---------------------------------------------------------------------- first-run setup

struct Setup {
    core: Entity<Core>,
    step: usize,
    mics: Entity<DeviceSelect>,
    every_app: bool,
    _subs: Vec<Subscription>,
}

const SETUP_TITLES: [&str; 3] = ["Choose your microphone", "Check your level", "Where will you use it?"];

fn open_setup(core: Entity<Core>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let c = core.read(cx);
        let items: Vec<SharedString> = c.inputs.iter().map(|s| SharedString::from(s.clone())).collect();
        let index = c.input.as_ref().and_then(|i| c.inputs.iter().position(|n| n == i)).map(|i| IndexPath::default().row(i));
        let mics = cx.new(|cx| SelectState::new(items, index, window, cx));
        let subs = vec![
            cx.subscribe(&mics, |this: &mut Setup, _, ev: &SelectEvent<Vec<SharedString>>, cx| {
                if let SelectEvent::Confirm(Some(name)) = ev {
                    this.core.update(cx, |c, cx| {
                        c.pick_input(name.to_string());
                        cx.notify();
                    });
                }
            }),
            // Redraw with the live level.
            cx.observe(&core, |_, _, cx| cx.notify()),
        ];
        Setup { core: core.clone(), step: 0, mics, every_app: true, _subs: subs }
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let setup = view.clone();
        dialog
            .w(px(500.))
            .child(view.clone())
            // Enter does what Next / Finish does.
            .on_ok(move |_, window, cx| {
                setup.update(cx, |s, cx| s.next(window, cx));
                false
            })
            .on_close(|_, _, _| crate::prefs::Prefs::mark_setup_done())
    });
}

impl Setup {
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step < 2 {
            self.step += 1;
            cx.notify();
            return;
        }
        crate::prefs::Prefs::mark_setup_done();
        let every = self.every_app;
        self.core.update(cx, |c, cx| {
            if c.virtual_mic_error.is_none() {
                if let Some(sink) = c.outputs.iter().find(|n| mw_engine::audio::is_send_device(n)).cloned() {
                    if c.send.as_deref() != Some(sink.as_str()) {
                        c.pick_send(sink);
                    }
                }
                if every != c.prefs.default_mic {
                    c.set_default_mic(every);
                }
            }
            cx.notify();
        });
        window.close_dialog(cx);
    }
}

impl Render for Setup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.core.read(cx);
        let (verdict, kind) = c.mic_verdict();
        let level = core::fraction(c.hold);
        let virtual_mic_ok = c.virtual_mic_error.is_none();
        let step = self.step;
        let body = match step {
            0 => v_flex().gap_3().child(muted("This is the mic the app listens to.")).child(Select::new(&self.mics).id("setup-mic")),
            1 => v_flex()
                .gap_3()
                .child(muted("Say a sentence at your normal volume."))
                .child(Progress::new("setup-level").value(level * 100.).color(kind_color(kind)))
                .child(div().text_size(px(14.)).child(verdict)),
            _ => {
                let choice = |id: &'static str, title: &'static str, hint: &'static str, on: bool, cx: &mut Context<Self>| {
                    div()
                        .id(id)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.every_app = id == "every-app";
                            cx.notify();
                        }))
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(if on { cx.theme().primary } else { cx.theme().border })
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(muted(hint))
                };
                let every = self.every_app;
                let mut v = v_flex().gap_3().child(muted("Other apps pick up your enhanced voice from MicWright Virtual Mic."));
                if virtual_mic_ok {
                    v = v
                        .child(choice(
                            "every-app",
                            "Apps using the default mic (recommended)",
                            "Makes the enhanced voice your default mic, so apps set to Default use it. MicWright also starts at login, so this keeps working after a restart.",
                            every,
                            cx,
                        ))
                        .child(choice("chosen-apps", "Only the apps I choose", "Pick MicWright Virtual Mic as the mic in Discord, OBS or your game. Your default mic stays as it is.", !every, cx));
                } else {
                    v = v.child(Alert::warning(
                        "no-virtual-mic",
                        "MicWright couldn't create its virtual mic. It needs a PipeWire or PulseAudio sound server, which every current desktop has. You can still hear yourself and record A/B samples.",
                    ));
                }
                v
            }
        };
        v_flex()
            .gap_4()
            .child(div().text_lg().font_weight(FontWeight::BOLD).child(format!("{} ({} of 3)", SETUP_TITLES[step], step + 1)))
            .child(body)
            .child(
                h_flex()
                    .justify_between()
                    .child(Button::new("skip").ghost().label("Skip").on_click(|_, window, cx| {
                        crate::prefs::Prefs::mark_setup_done();
                        window.close_dialog(cx);
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .when(step > 0, |el| {
                                el.child(Button::new("back").label("Back").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.step -= 1;
                                    cx.notify();
                                })))
                            })
                            .child(
                                Button::new("next")
                                    .primary()
                                    .label(if step == 2 { "Finish" } else { "Next" })
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.next(window, cx))),
                            ),
                    ),
            )
    }
}
