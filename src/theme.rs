use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, px, rgb};

/// Diffy always opens dark: a reviewer reads code all day, and a black window
/// sits next to the editor instead of flashing white between it.
pub fn install(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        // A step darker than the shipped dark theme so the window reads as
        // black, not gray, while raised surfaces keep their contrast.
        theme.background = shade(0x050506);
        theme.sidebar = shade(0x0B0B0F);
        theme.title_bar = shade(0x0B0B0F);
        theme.status_bar = shade(0x0B0B0F);
        theme.colors.list = shade(0x050506);
        theme.popover = shade(0x121216);
        theme.group_box = shade(0x0A0A0C);
        theme.input = shade(0x26262E);
        theme.muted = shade(0x1A1A1E);
        theme.secondary = shade(0x141417);
        theme.secondary_hover = shade(0x1C1C21);
        theme.secondary_active = shade(0x24242A);
        theme.border = shade(0x26262C);
        theme.sidebar_border = shade(0x1E1E24);
        theme.status_bar_border = shade(0x1E1E24);
        theme.list_hover = shade(0x121215);
        // The black canvas swallows the shipped neutral ring; a cool blue keeps
        // the keyboard focus visible on fields and buttons.
        theme.ring = shade(0x6E9BFF);
        theme.colors.list_active = shade(0x1A1F29);
        theme.colors.list_active_border = shade(0x2C3442);
        theme.radius = px(7.);
        theme.radius_lg = px(11.);
    });
}

fn shade(value: u32) -> Hsla {
    rgb(value).into()
}
