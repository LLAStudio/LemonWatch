use egui::{Color32, Style};

pub const LEMON: Color32 = Color32::from_rgb(250, 204, 21);
pub const LIME: Color32 = Color32::from_rgb(132, 204, 22);
pub const CORAL: Color32 = Color32::from_rgb(239, 68, 68);
pub const BG_DARK: Color32 = Color32::from_rgb(15, 17, 21);
pub const BG_CARD: Color32 = Color32::from_rgb(22, 25, 32);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(226, 232, 240);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(40, 46, 56);

/// Spacing plus the lemon accents, deliberately leaving the base visuals alone:
/// overriding them would repaint every panel and fight the colours the UI sets
/// on its own widgets.
pub fn apply_theme(style: &mut Style) {
    style.spacing.window_margin = egui::Margin::same(12.0);
    style.spacing.button_padding = egui::vec2(6.0, 4.0);
    style.visuals.hyperlink_color = LEMON;
    style.visuals.selection.bg_fill = LEMON.gamma_multiply(0.35);
    style.visuals.selection.stroke.color = LEMON;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_adds_spacing_and_lemon_accents_only() {
        let mut style = Style::default();
        let base_panel_fill = style.visuals.panel_fill;
        let base_widget_fill = style.visuals.widgets.inactive.bg_fill;

        apply_theme(&mut style);

        assert_eq!(
            style.visuals.panel_fill, base_panel_fill,
            "the base panel fill must be left to the platform theme"
        );
        assert_eq!(
            style.visuals.widgets.inactive.bg_fill, base_widget_fill,
            "widget visuals must not be repainted"
        );
        assert_eq!(style.visuals.hyperlink_color, LEMON);
        assert_eq!(style.visuals.selection.bg_fill, LEMON.gamma_multiply(0.35));
        assert_eq!(style.visuals.selection.stroke.color, LEMON);
        assert_eq!(style.spacing.button_padding, egui::vec2(6.0, 4.0));
        assert_eq!(style.spacing.window_margin, egui::Margin::same(12.0));
    }
}
