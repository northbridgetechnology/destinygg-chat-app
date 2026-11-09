mod account;
mod app;
mod assets;
mod channels;
mod chat;
mod drawer;
mod player;

use eframe::egui;

fn main() -> eframe::Result {
    #[cfg(target_os = "linux")]
    {
        gtk::gdk::set_allowed_backends("x11");
        gtk::init().map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DGG Desktop")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([890.0, 580.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DGG Desktop",
        options,
        Box::new(|cc| Ok(Box::new(app::Desktop::new(cc)?))),
    )
}
