//! The look of the Windows app (ui-winui/App.xaml): always dark, one purple accent, flat
//! borderless cards, quiet labels over bold values. The tokens below are that file's.

use crate::core::Kind;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub const BACKGROUND: u32 = 0x000000;
pub const CARD: u32 = 0x2A2A2C;
pub const CARD_HOVER: u32 = 0x333336;
pub const CARD_PRESSED: u32 = 0x242426;
pub const ACCENT: u32 = 0x8B7CF6;
pub const MUTED: u32 = 0xA3A3A8;
const FOREGROUND: u32 = 0xFFFFFF;
/// The selected and hovered sidebar entry.
pub const NAV_SELECTED: u32 = 0x1C1C1E;
pub const NAV_HOVER: u32 = 0x141416;

pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Repaints the component library in the Windows app's colours. Call after any
/// `Theme::change`, which resets them.
pub fn apply(cx: &mut App) {
    // GPUI's own Linux UI font is IBM Plex Sans in one weight, so bold text would come
    // out regular. The desktop's font has its weights, and looks native too.
    #[cfg(target_os = "linux")]
    let font = desktop_font();
    // `update` also refreshes the buttons' copies of the colours and every window.
    Theme::update(cx, |theme| {
        #[cfg(target_os = "linux")]
        if let Some(family) = font {
            theme.font_family = family.into();
        }
        paint(&mut theme.colors);
    });
}

fn paint(c: &mut gpui_kit::component::ThemeColor) {
    c.background = color(BACKGROUND).into();
    c.foreground = color(FOREGROUND).into();
    c.muted = color(CARD).into();
    c.muted_foreground = color(MUTED).into();
    c.border = color(0x3A3A3D).into();
    c.input = color(0x3A3A3D).into();
    c.ring = color(ACCENT).into();
    c.selection = color(ACCENT).opacity(0.35).into();
    c.caret = color(ACCENT).into();
    c.link = color(0xB4A9FA).into();
    c.link_hover = color(0xC9C1FB).into();
    c.link_active = color(ACCENT).into();
    // WinUI accent controls: purple with dark text.
    c.primary = color(ACCENT).into();
    c.primary_hover = color(0x9D90F8).into();
    c.primary_active = color(0x7464E0).into();
    c.primary_foreground = color(0x000000).into();
    c.button_primary = color(ACCENT).into();
    c.button_primary_hover = color(0x9D90F8).into();
    c.button_primary_active = color(0x7464E0).into();
    c.button_primary_foreground = color(0x000000).into();
    // Standard buttons: a step lighter than the cards they sit on.
    c.button = color(0x37373A).into();
    c.button_hover = color(0x404044).into();
    c.button_active = color(0x2F2F32).into();
    c.button_foreground = color(FOREGROUND).into();
    c.secondary = color(0x37373A).into();
    c.secondary_hover = color(0x404044).into();
    c.secondary_active = color(0x2F2F32).into();
    c.secondary_foreground = color(FOREGROUND).into();
    c.accent = color(0x38383B).into();
    c.accent_foreground = color(FOREGROUND).into();
    c.popover = color(0x2C2C2E).into();
    c.popover_foreground = color(FOREGROUND).into();
    c.list = color(0x2C2C2E).into();
    c.list_hover = color(0x38383B).into();
    c.list_active = color(ACCENT).opacity(0.25).into();
    c.list_active_border = color(ACCENT).into();
    c.switch = color(0x4A4A4F).into();
    c.switch_thumb = color(FOREGROUND).into();
    c.slider_bar = color(ACCENT).into();
    c.slider_thumb = color(ACCENT).into();
    c.progress_bar = color(ACCENT).into();
    c.tab_bar = color(BACKGROUND).into();
    c.tab = color(BACKGROUND).into();
    c.tab_active = color(BACKGROUND).into();
    c.tab_foreground = color(MUTED).into();
    c.tab_active_foreground = color(FOREGROUND).into();
    c.success = color(0x34D27B).into();
    c.warning = color(0xF5C542).into();
    c.danger = color(0xF87171).into();
    c.scrollbar = color(BACKGROUND).into();
    c.scrollbar_thumb = color(0x48484C).into();
    c.scrollbar_thumb_hover = color(0x5A5A5F).into();
    c.overlay = color(BACKGROUND).opacity(0.6).into();
}

/// Re-applies the palette if something reset the theme, as opening a window does when the
/// component library syncs with the system's light or dark mode.
pub fn keep(cx: &mut App) {
    if Hsla::from(Theme::global(cx).colors.primary) != color(ACCENT) {
        apply(cx);
    }
}

/// The desktop's sans-serif family, as fontconfig resolves it.
#[cfg(target_os = "linux")]
fn desktop_font() -> Option<String> {
    let out = std::process::Command::new("fc-match").args(["--format=%{family[0]}", "sans-serif"]).output().ok()?;
    let family = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !family.is_empty()).then_some(family)
}

/// Pill background and text, as in App.xaml.
fn pill_colors(kind: Kind) -> (u32, u32) {
    match kind {
        Kind::Good => (0x123A22, 0x34D27B),
        Kind::Warn => (0x3D3212, 0xF5C542),
        Kind::Bad => (0x40191B, 0xF87171),
        Kind::Off => (0x3A3A3D, 0xC8C8CC),
    }
}

pub fn kind_color(kind: Kind) -> Hsla {
    color(pill_colors(kind).1)
}

pub fn pill(text: impl Into<SharedString>, kind: Kind) -> Div {
    let (bg, fg) = pill_colors(kind);
    div()
        .flex_none()
        .px(px(10.))
        .py(px(3.))
        .rounded(px(11.))
        .bg(color(bg))
        .text_color(color(fg))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn icon(name: IconName, size: f32, tint: Hsla) -> Icon {
    Icon::new(name).with_size(px(size)).text_color(tint)
}

/// A flat card.
pub fn card() -> Div {
    v_flex().bg(color(CARD)).rounded(px(12.)).px(px(16.)).py(px(14.))
}

/// A whole card you can press: the card, plus hover and press.
pub fn tile(id: impl Into<ElementId>) -> Stateful<Div> {
    v_flex()
        .id(id)
        .bg(color(CARD))
        .rounded(px(12.))
        .px(px(16.))
        .py(px(14.))
        .cursor_pointer()
        .hover(|s| s.bg(color(CARD_HOVER)))
        .active(|s| s.bg(color(CARD_PRESSED)))
}

pub fn page_title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

/// Small caps-style label above a value ("INPUT DEVICE").
pub fn card_label(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(color(MUTED)).child(text.into())
}

pub fn card_value(text: impl Into<SharedString>) -> Div {
    div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).truncate().child(text.into())
}

pub fn tile_title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

pub fn muted(text: impl Into<SharedString>) -> Div {
    div().text_size(px(13.)).text_color(color(MUTED)).child(text.into())
}

/// A label that starts a group of cards ("VOLUME").
pub fn section(text: impl Into<SharedString>) -> Div {
    card_label(text).mt(px(20.)).mb(px(0.)).ml(px(1.))
}

/// WinUI's SettingsCard: icon, header and description on the left, the control on the right.
pub fn settings_card(icon_name: Option<IconName>, header: impl Into<SharedString>, description: impl Into<SharedString>, control: impl IntoElement) -> Div {
    settings_row(icon_name, header, description, control).bg(color(CARD)).rounded(px(12.)).px(px(16.)).py(px(14.)).min_h(px(68.))
}

/// A settings card's content without the card, for rows inside other cards.
pub fn settings_row(icon_name: Option<IconName>, header: impl Into<SharedString>, description: impl Into<SharedString>, control: impl IntoElement) -> Div {
    let description: SharedString = description.into();
    h_flex()
        .gap(px(16.))
        .items_center()
        .when_some(icon_name, |el, name| el.child(icon(name, 16., color(FOREGROUND))))
        // min_w_0 lets the text wrap instead of pushing the control out of the window.
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_size(px(14.)).child(header.into()))
                .when(!description.is_empty(), |el| el.child(div().text_size(px(12.)).text_color(color(MUTED)).child(description))),
        )
        .child(div().flex_none().child(control))
}
