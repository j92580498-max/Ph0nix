// Derived from tayga16/SamsungE380-Emu; original Apache-2.0/MIT terms retained.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod branding;
mod developer;
mod files;

fn main() -> anyhow::Result<()> {
    let root = std::env::current_exe()?.parent().unwrap().to_path_buf();
    let root = if root.join("firmware").is_dir() || root.join("engine").is_dir() {
        root
    } else {
        std::env::current_dir()?
    };
    let settings_dir = k750_native::worker::HostOptions::app_data(&root);
    let config = app::Settings::load(&settings_dir);
    let auto_run = std::env::args().any(|argument| argument == "--run");
    let native_options = eframe::NativeOptions {
        vsync: true,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([640.0, 420.0])
            .with_fullscreen(config.fullscreen)
            .with_icon(branding::icon())
            .with_title("ph0nix"),
        ..Default::default()
    };
    eframe::run_native(
        "ph0nix",
        native_options,
        Box::new(move |_cc| {
            let mut launcher = app::PhoneLauncher::new(root, config).with_settings_dir(settings_dir);
            if auto_run { launcher.run_selected_firmware(); }
            Box::new(launcher)
        }),
    ).map_err(|error| anyhow::anyhow!("eframe: {error}"))
}
