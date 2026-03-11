use bevy_egui::egui;
use std::sync::Arc;

/// Material Symbols font family name.
const ICON_FONT_FAMILY: &str = "material_symbols";

/// Random colors toggle (palette).
pub(crate) const ICON_CASINO: &str = "\u{e40a}";
/// Bounding box toggle (view_in_ar).
pub(crate) const ICON_BOUNDING_BOX: &str = "\u{e97a}";
/// Wireframe toggle (deployed_code).
pub(crate) const ICON_WIREFRAME: &str = "\u{e1af}";
/// Edge curves toggle (timeline).
pub(crate) const ICON_EDGES: &str = "\u{e922}";

/// Configure egui fonts with embedded Material Symbols subset.
pub(crate) fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        ICON_FONT_FAMILY.to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/MaterialSymbolsOutlined.ttf"
        ))),
    );

    // Add as fallback to Proportional so icon codepoints render anywhere.
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .push(ICON_FONT_FAMILY.to_owned());

    ctx.set_fonts(fonts);
}

/// Create styled icon text from a Material Symbols codepoint.
pub(crate) fn icon_text(icon: &str) -> egui::RichText {
    egui::RichText::new(icon).size(20.0)
}
