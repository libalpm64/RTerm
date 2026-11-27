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
