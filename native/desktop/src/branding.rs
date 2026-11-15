// Original ph0nix pixel artwork; exported from assets/branding/logo.pixels.json.
pub fn icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/branding/logo-icon.png"))
        .expect("embedded phone icon is a valid PNG")
}
