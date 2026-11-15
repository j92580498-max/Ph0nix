// Original ph0nix pixel artwork; exported from assets/branding/phoenix.pixels.json.
pub fn icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/branding/phoenix-icon.png"))
        .expect("embedded phoenix icon is a valid PNG")
}
