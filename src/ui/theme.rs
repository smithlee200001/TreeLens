//! Shared presentation palette and action bindings.
use super::{CancelScan, FocusSearch, OpenFolder, Quit, Refresh};
use crate::platform::desktop;
use gpui_kit::{component::Theme, *};

pub(super) const BG: u32 = 0x0b1017;
pub(super) const PANEL: u32 = 0x111923;
pub(super) const BORDER: u32 = 0x243241;
pub(super) const TEXT: u32 = 0xe8f1f8;
pub(super) const MUTED: u32 = 0x8fa3b8;
pub(super) const CYAN: u32 = 0x69e0d3;
pub(super) const LIME: u32 = 0xc4ed9c;
pub(super) const AMBER: u32 = 0xf6c77b;
pub(super) const PALETTE: [(u32, u32); 8] = [
    (0x194c4b, 0x69e0d3),
    (0x253f5b, 0x8dbdfb),
    (0x43375c, 0xc4a4f7),
    (0x485337, 0xc4ed9c),
    (0x584332, 0xf6c77b),
    (0x50364b, 0xeab3d6),
    (0x244b56, 0x8fdae9),
    (0x3d475a, 0xc4d1e4),
];

pub fn install_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.font_size = px(14.0);
        theme.radius = px(9.0);
        theme.colors.background = rgb(BG).into();
        theme.colors.foreground = rgb(TEXT).into();
        theme.colors.border = rgb(BORDER).into();
        theme.colors.input = rgb(0x0c141e).into();
        theme.colors.primary = rgb(CYAN).into();
        theme.colors.button_primary = rgb(CYAN).into();
        theme.colors.button_primary_hover = rgb(0x91eee3).into();
        theme.colors.button_primary_active = rgb(0x4cc3b7).into();
        theme.colors.button_primary_foreground = rgb(0x092322).into();
        theme.colors.button = rgb(0x1a2634).into();
        theme.colors.button_hover = rgb(0x26394a).into();
        theme.colors.button_foreground = rgb(TEXT).into();
        theme.colors.accent = rgb(0x213b40).into();
        theme.colors.accent_foreground = rgb(CYAN).into();
    });
    cx.bind_keys([
        KeyBinding::new(desktop::OPEN_SHORTCUT, OpenFolder, None),
        KeyBinding::new(desktop::REFRESH_SHORTCUT, Refresh, None),
        KeyBinding::new(desktop::SEARCH_SHORTCUT, FocusSearch, None),
        KeyBinding::new("escape", CancelScan, Some("TreeLens")),
        KeyBinding::new(desktop::QUIT_SHORTCUT, Quit, None),
    ]);
}
