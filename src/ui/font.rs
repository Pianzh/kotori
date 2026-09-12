//! Font constants and the one helper that names the bundled font.

const UI_FONT_FAMILY: &str = "Kotori Sans";
pub(super) const UI_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/KotoriSans-Regular.ttf");

pub(super) fn ui_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(UI_FONT_FAMILY),
        ..Default::default()
    }
}
