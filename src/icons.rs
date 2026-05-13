use bevy_egui::egui;
use std::sync::Arc;

/// Material Symbols font family name.
const ICON_FONT_FAMILY: &str = "material_symbols";

/// Random colors toggle (casino).
pub(crate) const ICON_CASINO: &str = "\u{eb40}";
/// STEP colors toggle (palette).
pub(crate) const ICON_PALETTE: &str = "\u{e40a}";
/// Bounding box toggle (view_in_ar).
pub(crate) const ICON_BOUNDING_BOX: &str = "\u{efc9}";
/// Wireframe toggle (deployed_code).
pub(crate) const ICON_WIREFRAME: &str = "\u{f720}";
/// Edge curves toggle (timeline). Currently unused after the toolbar icon
/// swap; kept in case it's reused.
#[allow(dead_code)]
pub(crate) const ICON_EDGES: &str = "\u{e922}";
/// Polygon-edges toggle (details).
pub(crate) const ICON_DETAILS: &str = "\u{e3c8}";
/// Isoparams overlay toggle (grid_on).
pub(crate) const ICON_GRID_ON: &str = "\u{e3ec}";
/// NSI overlay toggle (counter_3).
#[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
pub(crate) const ICON_COUNTER_3: &str = "\u{f782}";

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
