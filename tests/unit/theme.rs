use super::*;

#[test]
fn compact_list_scrolls_without_reserving_scrollbar_space() {
    let context = egui::Context::default();
    let mut viewport = Rect::NOTHING;
    let mut offset = 0.0;
    let mut content = 0.0;
    let mut render = |events| {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 300.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let width = ui.available_width();
                    let result = compact_list(90.0).show(ui, |ui| {
                        for _ in 0..50 {
                            ui.allocate_exact_size(
                                vec2(ui.available_width(), 18.0),
                                Sense::hover(),
                            );
                        }
                    });
                    assert!((result.inner_rect.width() - width).abs() < 1.0);
                    assert!(result.inner_rect.height() <= 90.0);
                    viewport = result.inner_rect;
                    offset = result.state.offset.y;
                    content = result.content_size.y;
                });
            },
        );
        (viewport, offset, content)
    };
    let (viewport, offset, content) = render(vec![]);
    assert_eq!(offset, 0.0);
    assert!(content > viewport.height());
    let (_, offset, _) = render(vec![
        egui::Event::PointerMoved(viewport.center()),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -160.0),
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    assert!(
        offset > 0.0,
        "overflow must scroll even with hidden scrollbars"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_theme_requests_dark_window_decorations() {
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        Theme::dark().apply(context);
    });
    assert_eq!(context.theme(), egui::Theme::Dark);
    assert!(
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(
                command,
                egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark)
            ))
    );
}
