use crate::store::{self, ThemePref};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, px, rgb};

/// Installs the theme saved in settings (light by default): a white window
/// with an orange accent in light mode, a near-black one in dark mode. One
/// ground, one ink, one accent; chrome surfaces step one tone off the ground
/// instead of drawing rules around themselves.
pub fn install(cx: &mut App) {
    let pref = store::settings().map(|s| s.theme).unwrap_or_default();
    apply(pref, cx);
}

/// Switches the window to the chosen theme and reapplies Diffy's palette.
pub fn apply(pref: ThemePref, cx: &mut App) {
    let mode = match pref {
        ThemePref::Light => ThemeMode::Light,
        ThemePref::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| match mode {
        ThemeMode::Light => apply_light(theme),
        ThemeMode::Dark => apply_dark(theme),
    });
}

/// Light palette: white ground, near-black ink, orange accent. The accent
/// sits on the primary action, links, the focus ring, the text caret, and
/// the selected row. Orange-700 keeps white text at 5.2:1; the ring uses
/// orange-600, 3.6:1 against white.
fn apply_light(theme: &mut Theme) {
    // Grounds: the canvas, popovers and code are white; the panels, top
    // strip, and the detail header sit one step off it.
    theme.background = shade(0xFFFFFF);
    theme.foreground = shade(0x171717);
    theme.sidebar = shade(0xFAFAFA);
    theme.sidebar_foreground = shade(0x171717);
    theme.title_bar = shade(0xFAFAFA);
    theme.popover = shade(0xFFFFFF);
    theme.popover_foreground = shade(0x171717);
    theme.colors.list = shade(0xFAFAFA);
    theme.group_box = shade(0xF5F5F5);
    theme.group_box_foreground = shade(0x171717);
    theme.muted = shade(0xF5F5F5);
    theme.muted_foreground = shade(0x6B6B6B);
    theme.secondary = shade(0xF5F5F5);
    theme.secondary_foreground = shade(0x171717);
    theme.secondary_hover = shade(0xECECEC);
    theme.secondary_active = shade(0xE5E5E5);
    // Structure: one hairline tone for region seams, one stronger tone for
    // control borders.
    theme.border = shade(0xE5E5E5);
    theme.sidebar_border = shade(0xE5E5E5);
    theme.input = shade(0xD4D4D4);
    theme.list_hover = shade(0xF0F0F0);
    // The accent is orange.
    theme.primary = shade(0xC2410C);
    theme.primary_hover = shade(0x9A3412);
    theme.primary_active = shade(0x7C2D12);
    theme.primary_foreground = shade(0xFFFFFF);
    theme.ring = shade(0xEA580C);
    theme.link = shade(0xC2410C);
    theme.link_hover = shade(0x9A3412);
    theme.link_active = shade(0x7C2D12);
    theme.caret = shade(0xEA580C);
    theme.progress_bar = shade(0xEA580C);
    theme.selection = shade(0xFED7AA);
    // The selected row: a soft orange fill with an orange edge.
    theme.colors.list_active = shade(0xFFEDD5);
    theme.colors.list_active_border = shade(0xFDBA74);
    // Status colors double as text on white, so they are the dark steps of
    // their hues (all at or above 5:1 on white).
    theme.success = shade(0x15803D);
    theme.success_foreground = shade(0xFFFFFF);
    theme.danger = shade(0xB42318);
    theme.danger_foreground = shade(0xFFFFFF);
    theme.warning = shade(0xB45309);
    theme.warning_foreground = shade(0xFFFFFF);
    theme.radius = px(7.);
    theme.radius_lg = px(11.);
}

/// Dark palette: near-black ground, light ink, the same orange accent
/// brightened for dark surfaces. The primary fill is orange-600 with a
/// near-black label (5.9:1); status colors are the light steps of their
/// hues, all at or above 5.7:1 on the ground.
fn apply_dark(theme: &mut Theme) {
    theme.background = shade(0x050506);
    theme.foreground = shade(0xEDEDED);
    theme.sidebar = shade(0x0B0B0F);
    theme.sidebar_foreground = shade(0xEDEDED);
    theme.title_bar = shade(0x0B0B0F);
    theme.popover = shade(0x121216);
    theme.popover_foreground = shade(0xEDEDED);
    theme.colors.list = shade(0x050506);
    theme.group_box = shade(0x0A0A0C);
    theme.group_box_foreground = shade(0xEDEDED);
    theme.muted = shade(0x1A1A1E);
    theme.muted_foreground = shade(0xA1A1AA);
    theme.secondary = shade(0x141417);
    theme.secondary_foreground = shade(0xEDEDED);
    theme.secondary_hover = shade(0x1C1C21);
    theme.secondary_active = shade(0x24242A);
    theme.border = shade(0x26262C);
    theme.sidebar_border = shade(0x1E1E24);
    theme.input = shade(0x26262E);
    theme.list_hover = shade(0x121215);
    theme.primary = shade(0xEA580C);
    theme.primary_hover = shade(0xF97316);
    theme.primary_active = shade(0xC2410C);
    theme.primary_foreground = shade(0x000000);
    theme.ring = shade(0xEA580C);
    theme.link = shade(0xFB923C);
    theme.link_hover = shade(0xFDBA74);
    theme.link_active = shade(0xEA580C);
    theme.caret = shade(0xEA580C);
    theme.progress_bar = shade(0xEA580C);
    theme.selection = shade(0x7C2D12);
    // The selected row: a dark orange fill with an orange edge.
    theme.colors.list_active = shade(0x331D0E);
    theme.colors.list_active_border = shade(0x9A3412);
    theme.success = shade(0x3FC07A);
    theme.success_foreground = shade(0x050506);
    theme.danger = shade(0xE5566F);
    theme.danger_foreground = shade(0x050506);
    theme.warning = shade(0xFBBF24);
    theme.warning_foreground = shade(0x050506);
    theme.radius = px(7.);
    theme.radius_lg = px(11.);
}

fn shade(value: u32) -> Hsla {
    rgb(value).into()
}
