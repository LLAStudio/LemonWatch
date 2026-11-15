use egui::{Color32, Style, Visuals};

pub const LEMON: Color32 = Color32::from_rgb(250, 204, 21);
pub const LIME: Color32 = Color32::from_rgb(132, 204, 22);
pub const CORAL: Color32 = Color32::from_rgb(239, 68, 68);
pub const BG_DARK: Color32 = Color32::from_rgb(15, 17, 21);
pub const BG_CARD: Color32 = Color32::from_rgb(22, 25, 32);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(226, 232, 240);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(40, 46, 56);

// Not wired into the window yet: the UI paints its own colours and switching the
// global visuals on would restyle the whole app, so this stays a ready-made hook.
#[allow(dead_code)]
pub fn apply_theme(style: &mut Style) {
    style.spacing.window_margin = egui::Margin::same(12.0);
    style.spacing.button_padding = egui::vec2(6.0, 4.0);
    style.visuals = Visuals {
        dark_mode: true,
        panel_fill: BG_DARK,
        window_fill: BG_DARK,
        faint_bg_color: BG_DARK,
        hyperlink_color: LEMON,
        ..Default::default()
    };
}
