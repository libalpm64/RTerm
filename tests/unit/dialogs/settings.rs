use super::*;
use crate::egui_test::TestContextExt;

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
        let output = ctx.run_test_pass(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(width, 400.0),
                )),
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        setting_row(ui, t, first, |ui| ui.button("Toggle"));
                        setting_row(ui, t, second, |ui| ui.button("Toggle"));
                    })
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
        let output = ctx.run_test_pass(
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
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        bounds = ui.available_rect_before_wrap();
                        let result =
                            settings_scroll(ui, t, 1, 180.0, |ui| settings.appearance_page(ui, t));
                        inner = result.inner_rect;
                        offset = result.state.clamped_offset().y;
                        assert!(result.content_size.y > inner.height());
                    })
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
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
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

#[test]
fn software_update_is_a_dedicated_settings_page() {
    let context = egui::Context::default();
    let mut settings = Settings::benchmark_defaults();
    let mut password = String::new();
    let mut seen = false;
    let render = |settings: &mut Settings, password: &mut String, seen: &mut bool, events| {
        context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(710.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        settings.show(ui, Theme::dark(), password, false, |_, section| {
                            *seen |= matches!(section, SettingsSection::Update);
                        });
                    })
                });
            },
        )
    };
    let output = render(&mut settings, &mut password, &mut seen, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Update" => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .expect("settings should display the Update category");
    for pressed in [true, false] {
        render(
            &mut settings,
            &mut password,
            &mut seen,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(settings.page, 7);
    assert!(seen);
}

#[test]
fn settings_sidebar_reaches_header_edge_and_aligns_labels() {
    let context = egui::Context::default();
    theme::install_fonts(&context);
    let t = Theme::dark();
    t.apply(&context);
    let mut settings = Settings::benchmark_defaults();
    let mut password = String::new();
    settings.page = 7;
    for width in [520.0, 710.0] {
        let render = |settings: &mut Settings, password: &mut String, events| {
            context.run_test_pass(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(1000.0, 720.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    super::super::modal("settings-sidebar-fixture", t).show(ctx, |ui| {
                        super::super::modal_header(ui, "Settings", t, width);
                        settings.show(ui, t, password, false, |_, _| {});
                    });
                },
            )
        };
        // Allow the modal to settle its measured size before inspecting geometry.
        for _ in 0..30 {
            render(&mut settings, &mut password, vec![]);
        }
        let output = render(&mut settings, &mut password, vec![]);
        let header = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == t.bar && (rect.rect.height() - 34.0).abs() < 0.5 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("settings header");
        let highlight = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == t.hover && (rect.rect.height() - 30.0).abs() < 0.5 =>
                {
                    Some((rect.rect, shape.clip_rect))
                }
                _ => None,
            })
            .expect("selected navigation row");
        assert!(
            (highlight.0.left() - header.left()).abs() < 0.5,
            "sidebar highlight must be flush with the header"
        );
        assert!(
            highlight.1.left() <= highlight.0.left() + 0.5,
            "sidebar must not be clipped by the modal inset"
        );
        let label = |name: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == name => Some(text.pos),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing label {name}"))
        };
        let title = label("Settings");
        let first = label("Terminal");
        assert!(
            (first.x - title.x).abs() < 0.5,
            "navigation and title text must align"
        );
        for (index, name) in [
            "Terminal",
            "Appearance",
            "Connection",
            "Security",
            "Import",
            "SFTP",
            "AI",
            "Update",
        ]
        .iter()
        .enumerate()
        {
            let pos = label(name);
            assert!(
                (pos.x - first.x).abs() < 0.5,
                "uneven horizontal padding: {name}"
            );
            assert!(
                (pos.y - first.y - index as f32 * 30.0).abs() < 0.5,
                "uneven row spacing: {name}"
            );
        }
        let pos = egui::pos2(header.left() + 5.0, highlight.0.center().y - 30.0);
        for pressed in [true, false] {
            render(
                &mut settings,
                &mut password,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(
            settings.page, 6,
            "the entire sidebar row should be clickable"
        );
        settings.page = 7;
    }
}
