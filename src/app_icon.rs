use std::sync::{Arc, OnceLock};

use eframe::egui;

pub(crate) fn window() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        let rgba = image::load_from_memory_with_format(
            include_bytes!("../assets/rterm.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded RTerm icon must be a valid PNG")
        .resize_exact(64, 64, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
        Arc::new(egui::IconData {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    })
    .clone()
}

#[cfg(test)]
#[path = "../tests/unit/app_icon.rs"]
mod tests;
