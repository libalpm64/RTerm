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
#[path = "../tests/unit/background.rs"]
mod tests;
