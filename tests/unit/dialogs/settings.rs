use super::*;

#[test]
fn legacy_background_preference_does_not_enable_interactive_execution() {
    let upgraded = Settings::from_store(|key| match key {
        "ai_background" => Some("0".into()),
        "ai_access" => Some("ask".into()),
        _ => None,
    });
    assert!(!upgraded.on("ai_interactive"));
    assert_eq!(upgraded.get("ai_access"), "ask");
    let opted_in = Settings::from_store(|key| (key == "ai_interactive").then(|| "1".into()));
    assert!(opted_in.on("ai_interactive"));
}

#[test]
fn settings_rows_fit_wrapped_descriptions() {
    check_wrapped_rows(
        "en",
        (
            "Wrapped setting",
            "A deliberately long setting description that wraps across several lines in a narrow settings panel.",
        ),
        ("Next setting", "Short description"),
    );
    for locale in ["de", "ru"] {
        check_wrapped_rows(
            locale,
            (
                "Reconnect SSH",
                "Check idle connections and retry disconnected tabs",
            ),
            ("Default Port", "Port for new SSH connections"),
        );
    }
}

fn check_wrapped_rows(locale: &'static str, first: (&str, &str), second: (&str, &str)) {
    let ctx = egui::Context::default();
    crate::i18n::set(&ctx, locale);
    theme::install_fonts(&ctx);
    let t = Theme::dark();
    t.apply(&ctx);
    let description = crate::i18n::text(&ctx, first.1);
    let next_title = crate::i18n::text(&ctx, second.0);
    for width in [260.0, 430.0, 650.0] {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(width, 400.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    setting_row(ui, t, first, |ui| ui.button("Toggle"));
                    setting_row(ui, t, second, |ui| ui.button("Toggle"));
                });
            },
        );
        let mut bottom = None;
        let mut next_top = None;
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                if text.galley.text() == description {
                    let end = text.pos.y + text.galley.size().y;
                    assert!(
                        end <= clipped.clip_rect.bottom() + 0.5,
                        "wrapped caption clipped at width {width}"
                    );
                    bottom = Some(end);
                } else if text.galley.text() == next_title {
                    next_top = Some(text.pos.y);
                }
            }
        }
        assert!(
            bottom.unwrap() < next_top.unwrap(),
            "settings overlap at width {width}"
        );
    }
}

#[test]
fn settings_scrollbar_reserves_space_and_clips_rows() {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    let t = Theme::dark();
    t.apply(&ctx);
    let mut settings = Settings::benchmark_defaults();
    let mut render = |time: f64, events: Vec<egui::Event>| {
        let mut bounds = egui::Rect::NOTHING;
        let mut inner = egui::Rect::NOTHING;
        let mut offset = 0.0;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(430.0, 260.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    bounds = ui.available_rect_before_wrap();
                    let result =
                        settings_scroll(ui, t, 1, 180.0, |ui| settings.appearance_page(ui, t));
                    inner = result.inner_rect;
                    offset = result.state.offset.y;
                    assert!(result.content_size.y > inner.height());
                });
            },
        );
        (bounds, inner, offset, output)
    };
    render(0.0, vec![]);
    render(0.2, vec![]);
    let (bounds, inner, _, _) = render(0.4, vec![]);
    assert!(
        bounds.right() - inner.right() >= 17.0,
        "scrollbar needs its own gutter"
    );
    let (_, inner, offset, output) = render(
        0.6,
        vec![
            egui::Event::PointerMoved(inner.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, -220.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert!(offset > 0.0);
    for shape in output.shapes {
        if let egui::Shape::Text(text) = &shape.shape
            && (text.galley.text() == "Background Image"
                || text.galley.text() == "Line Numbers"
                || text.galley.text() == "Custom Keywords")
        {
            assert!(shape.clip_rect.bottom() <= inner.bottom() + 3.0);
            assert!(shape.clip_rect.right() <= inner.right() + 1.0);
        }
    }
}

impl Settings {
    pub(crate) fn test_set_page(&mut self, page: usize) {
        self.page = page;
    }
}
