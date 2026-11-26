//! Optional terminal wallpaper. One shared, bounded texture; no work when off.
use std::{fs::File, io::BufReader, path::Path};

use eframe::egui::{self, Color32, Rect, pos2};
use image::{ImageFormat, ImageReader, Limits};

use crate::dialogs::Settings;

#[derive(Clone)]
pub(crate) struct Wallpaper {
    path: String,
    glass: bool,
    opacity: u8,
    texture: egui::TextureHandle,
}

impl Wallpaper {
    pub(crate) fn from_settings(
        ctx: &egui::Context,
        settings: &Settings,
        previous: Option<&Self>,
    ) -> Result<Option<Self>, String> {
        if !settings.on("background_enabled") {
            return Ok(None);
        }
        let path = settings.get("background_path");
        if path.is_empty() {
            return Err("Choose an image for the terminal background".into());
        }
        let glass = settings.on("background_glass");
        let opacity = settings.background_opacity();
        if let Some(old) = previous.filter(|old| old.path == path && old.glass == glass) {
            let mut wallpaper = old.clone();
            wallpaper.opacity = opacity;
            return Ok(Some(wallpaper));
        }
        // Blur and downsample once, not per frame. Keep even 4K wallpaper
        // textures small, particularly when the frosted effect is enabled.
        let edge = if glass { 960 } else { 1920 };
        let image = decode_image(Path::new(path), edge)?;
        let image = if image.width() > edge || image.height() > edge {
            image.thumbnail(edge, edge)
        } else {
            image
        };
        let image = if glass { image.blur(7.0) } else { image }.into_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        let texture = ctx.load_texture("terminal-wallpaper", pixels, egui::TextureOptions::LINEAR);
        Ok(Some(Self {
            path: path.into(),
            glass,
            opacity,
            texture,
        }))
    }

    pub(crate) fn paint(&self, painter: &egui::Painter, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 || self.opacity == 0 {
            return;
        }
        let uv = cover_uv(self.texture.size_vec2(), rect.size());
        painter.image(
            self.texture.id(),
            rect,
            uv,
            Color32::from_white_alpha((f32::from(self.opacity) * 2.55).round() as u8),
        );
    }

    pub(crate) fn visible(&self) -> bool {
        self.opacity > 0
    }
}

fn decode_image(path: &Path, edge: u32) -> Result<image::DynamicImage, String> {
    let file =
        File::open(path).map_err(|error| format!("Cannot open background image: {error}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("Background image must be 32 MiB or smaller".into());
    }
    let mut reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        let mut limits = Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        return reader
            .decode()
            .map_err(|e| format!("Invalid or oversized background image: {e}"));
    }
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if extension.eq_ignore_ascii_case("heic") || extension.eq_ignore_ascii_case("heif") {
        drop(reader);
        return decode_heif(path, edge);
    }
    Err("Unsupported background image. Select a PNG, JPG, JPEG, HEIF or HEIC file.".into())
}

#[cfg(not(windows))]
fn decode_heif(_: &Path, _: u32) -> Result<image::DynamicImage, String> {
    Err("HEIF/HEIC decoding currently requires Windows image codecs. Use PNG or JPEG on this platform.".into())
}

#[cfg(windows)]
fn decode_heif(path: &Path, edge: u32) -> Result<image::DynamicImage, String> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows::{
        Win32::{
            Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE},
            Graphics::Imaging::*,
            System::Com::*,
        },
        core::PCWSTR,
    };
    // COM may already be initialized as STA by the native file picker.
    // Balance only our own successful initialization, including S_FALSE.
    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if status.is_err() && status != RPC_E_CHANGED_MODE {
        return Err(format!(
            "Cannot initialize Windows image decoder: {status:?}"
        ));
    }
    let _com = ComGuard(status.is_ok());
    let path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let decoded = (|| -> windows::core::Result<image::DynamicImage> {
        // All COM objects live within this scope and are released before COM
        // is uninitialized. WIC uses the OS-installed HEIF/HEVC decoder.
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let decoder = factory.CreateDecoderFromFilename(
                PCWSTR(path.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;
            if w == 0
                || h == 0
                || w > 8192
                || h > 8192
                || u64::from(w) * u64::from(h) * 4 > 128 * 1024 * 1024
            {
                return Err(windows::core::Error::new(
                    windows::core::HRESULT(0x80070057u32 as i32),
                    "Background image dimensions exceed the limit",
                ));
            }
            let converter = factory.CreateFormatConverter()?;
            let scaler = factory.CreateBitmapScaler()?;
            if w.max(h) > edge {
                let longest = u64::from(w.max(h));
                w = ((u64::from(w) * u64::from(edge) / longest) as u32).max(1);
                h = ((u64::from(h) * u64::from(edge) / longest) as u32).max(1);
                scaler.Initialize(&frame, w, h, WICBitmapInterpolationModeFant)?;
                converter.Initialize(
                    &scaler,
                    &GUID_WICPixelFormat32bppRGBA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )?;
            } else {
                converter.Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppRGBA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )?;
            }
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            converter.CopyPixels(ptr::null(), w * 4, &mut pixels)?;
            Ok(image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(w, h, pixels).expect("validated RGBA dimensions"),
            ))
        }
    })();
    decoded.map_err(|e| format!("Cannot decode HEIF/HEIC image: {e}. Ensure Windows HEIF Image Extensions and HEVC Video Extensions are installed, and the image is valid."))
}

// Center-crop to cover the terminal without distorting the image.
fn cover_uv(image: egui::Vec2, pane: egui::Vec2) -> Rect {
    let ratio = (pane.x / pane.y) / (image.x / image.y);
    let (w, h) = if ratio < 1.0 {
        (ratio, 1.0)
    } else {
        (1.0, 1.0 / ratio)
    };
    Rect::from_min_max(
        pos2((1.0 - w) * 0.5, (1.0 - h) * 0.5),
        pos2((1.0 + w) * 0.5, (1.0 + h) * 0.5),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_wallpaper_decodes_and_rejects_invalid_images() {
        let dir = tempfile::tempdir().unwrap();
        let pixels = image::RgbImage::from_pixel(64, 32, image::Rgb([32, 128, 240]));
        for name in ["wallpaper.jpg", "wallpaper.jpeg", "wallpaper.JPG"] {
            let path = dir.path().join(name);
            pixels.save_with_format(&path, ImageFormat::Jpeg).unwrap();
            let decoded = decode_image(&path, 960).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (64, 32));
            let mut settings = Settings::benchmark_defaults();
            settings.set("background_enabled", "1".into());
            settings.set("background_path", path.to_string_lossy().into_owned());
            let wallpaper = Wallpaper::from_settings(&egui::Context::default(), &settings, None)
                .unwrap()
                .unwrap();
            assert_eq!(wallpaper.texture.size(), [64, 32]);
        }
        let invalid = dir.path().join("invalid.jpg");
        std::fs::write(&invalid, [0xff, 0xd8, 0xff, 0x00]).unwrap();
        assert!(decode_image(&invalid, 960).is_err());
        let unsupported = dir.path().join("wallpaper.txt");
        std::fs::write(&unsupported, b"not an image").unwrap();
        assert!(
            decode_image(&unsupported, 960)
                .unwrap_err()
                .contains("Unsupported")
        );
    }

    // Optional real fixture test: Windows codecs are not installed on every CI host.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires a genuine HEIF/HEIC fixture and installed Windows codecs"]
    fn windows_heif_wallpaper_decodes() {
        let path = std::path::PathBuf::from(
            std::env::var_os("RTERM_HEIF_FIXTURE").expect("set RTERM_HEIF_FIXTURE"),
        );
        let dir = tempfile::tempdir().unwrap();
        for extension in ["heic", "heif"] {
            let path_copy = dir.path().join(format!("wallpaper.{extension}"));
            std::fs::copy(&path, &path_copy).unwrap();
            let decoded = decode_image(&path_copy, 960).unwrap();
            assert!(decoded.width() > 0 && decoded.height() > 0);
            assert!(decoded.width().max(decoded.height()) <= 960);
            let mut settings = Settings::benchmark_defaults();
            settings.set("background_enabled", "1".into());
            settings.set("background_path", path_copy.to_string_lossy().into_owned());
            let wallpaper = Wallpaper::from_settings(&egui::Context::default(), &settings, None)
                .unwrap()
                .unwrap();
            assert!(wallpaper.texture.size().into_iter().max().unwrap() <= 960);
            println!("{extension}: {} x {}", decoded.width(), decoded.height());
        }
    }

    #[test]
    fn wallpaper_is_opt_in_and_reuses_texture() {
        let ctx = egui::Context::default();
        let mut settings = Settings::benchmark_defaults();
        assert!(
            Wallpaper::from_settings(&ctx, &settings, None)
                .unwrap()
                .is_none()
        );
        settings.set("background_path", "missing.png".into());
        assert!(
            Wallpaper::from_settings(&ctx, &settings, None)
                .unwrap()
                .is_none()
        );
        settings.set("background_enabled", "1".into());
        assert!(Wallpaper::from_settings(&ctx, &settings, None).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallpaper.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([32, 128, 255, 255]))
            .save(&path)
            .unwrap();
        settings.set("background_path", path.to_string_lossy().into_owned());
        let wallpaper = Wallpaper::from_settings(&ctx, &settings, None)
            .unwrap()
            .unwrap();
        assert_eq!(wallpaper.texture.size(), [64, 32]);
        settings.set("background_opacity", "40".into());
        let next = Wallpaper::from_settings(&ctx, &settings, Some(&wallpaper))
            .unwrap()
            .unwrap();
        assert_eq!(next.texture.id(), wallpaper.texture.id());
        assert_eq!(next.opacity, 40);
        settings.set("background_glass", "0".into());
        let sharp = Wallpaper::from_settings(&ctx, &settings, Some(&next))
            .unwrap()
            .unwrap();
        assert_ne!(sharp.texture.id(), next.texture.id());
        settings.set("background_enabled", "0".into());
        assert!(
            Wallpaper::from_settings(&ctx, &settings, Some(&sharp))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn terminal_keeps_wallpaper_in_normal_and_alternate_screens() {
        use crate::{
            backend::Backend,
            highlight::Highlighter,
            terminal::{Kind, Terminal},
        };
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut settings = Settings::benchmark_defaults();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallpaper.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([32, 128, 255, 255]))
            .save(&path)
            .unwrap();
        settings.set("background_path", path.to_string_lossy().into_owned());
        settings.set("background_enabled", "1".into());
        let wallpaper = Wallpaper::from_settings(&ctx, &settings, None)
            .unwrap()
            .unwrap();
        let (backend, _) = Backend::new(&ctx);
        let options = settings.terminal();
        let highlighter = Highlighter::new(false, "");
        let mut terminal = Terminal::new(1, "Wallpaper".into(), Kind::Local, &options);
        let check_cells = std::cell::Cell::new(false);
        let render = |terminal: &mut Terminal,
                      background: Option<&Wallpaper>,
                      line_numbers: bool,
                      preview: Option<f32>| {
            let options = crate::terminal::TerminalOptions {
                line_numbers,
                ..options
            };
            let mut viewport = Rect::NOTHING;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        pos2(0.0, 0.0),
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        viewport = ui.max_rect().with_max_x(ui.max_rect().right() - 100.0);
                        terminal.show(
                            ui,
                            viewport,
                            &backend,
                            &options,
                            &highlighter,
                            false,
                            preview,
                            background,
                        );
                    });
                },
            );
            let track = viewport.with_min_x(viewport.right() - 8.0);
            let opaque_track = output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == track && rect.fill == Color32::BLACK));
            if check_cells.get() {
                // Only the inverse cell should paint a black cell-sized quad.
                // The ordinary ANSI-black canvas must reveal the wallpaper.
                let cells: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if rect.rect.width() < 30.0 && rect.rect.height() < 50.0 =>
                        {
                            Some(rect.fill)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    cells
                        .iter()
                        .filter(|&&color| color == Color32::BLACK)
                        .count(),
                    1
                );
                assert!(cells.contains(&Color32::from_rgb(205, 0, 0)));
            }
            let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
            let mut found = false;
            for primitive in &meshes {
                if let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive
                    && mesh.texture_id == wallpaper.texture.id()
                {
                    found = true;
                    assert_eq!(mesh.calc_bounds(), viewport);
                    assert!(primitive.clip_rect.right() <= viewport.right());
                }
            }
            if found {
                assert!(
                    !opaque_track,
                    "wallpaper must not be covered by black track"
                );
            }
            (found, opaque_track)
        };
        assert_eq!(render(&mut terminal, None, true, None), (false, true));
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), true, None),
            (true, false)
        );
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), false, None),
            (true, false)
        );
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), true, Some(20.0)),
            (true, false)
        );
        terminal.receive(b"\x1b[?1049h");
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), true, None),
            (true, false)
        );
        // Curses-style screen: explicit black background, a colored cell,
        // and a black inverse cell. Preserve the latter two as opaque.
        terminal.receive(b"\x1b[40m\x1b[2J\x1b[Hblack\x1b[41mR\x1b[30;47;7mI\x1b[0m");
        check_cells.set(true);
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), true, None),
            (true, false)
        );
        check_cells.set(false);
        assert_eq!(render(&mut terminal, None, true, None), (false, false));
        terminal.receive(b"\x1b]11;#203040\x07");
        assert_eq!(
            render(&mut terminal, Some(&wallpaper), true, None),
            (false, false)
        );
        terminal.receive(b"\x1b]111\x07\x1b[?1049l");
        assert!(render(&mut terminal, Some(&wallpaper), true, None).0);
    }

    #[test]
    fn cover_preserves_aspect_ratio() {
        let uv = cover_uv(egui::vec2(200.0, 100.0), egui::vec2(100.0, 100.0));
        assert_eq!(uv, Rect::from_min_max(pos2(0.25, 0.0), pos2(0.75, 1.0)));
        let uv = cover_uv(egui::vec2(100.0, 200.0), egui::vec2(100.0, 100.0));
        assert_eq!(uv, Rect::from_min_max(pos2(0.0, 0.25), pos2(1.0, 0.75)));
    }
}
