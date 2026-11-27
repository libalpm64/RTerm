use super::*;
use egui::{Event, PointerButton, RawInput, Shape};

#[test]
fn terminal_context_menu_aligns_actions_and_uses_the_full_row() {
    for locale in ["en", "de", "ru"] {
        check_terminal_context_menu(locale);
    }
}

fn check_terminal_context_menu(locale: &'static str) {
    let context = egui::Context::default();
    crate::i18n::set(&context, locale);
    theme::install_fonts(&context);
    let theme = Theme::dark();
    theme.apply(&context);
    let mut selected = None;
    let mut frame = |events| {
        context.run(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 480.0))),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let response = ui.allocate_response(vec2(500.0, 400.0), Sense::click());
                    if let Some(action) = terminal_menu(egui::Popup::context_menu(&response), theme)
                    {
                        selected = Some(action);
                    }
                });
            },
        )
    };
    let pointer = |pos, button, pressed| Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let anchor = pos2(120.0, 140.0);
    frame(vec![Event::PointerMoved(anchor)]);
    frame(vec![pointer(anchor, PointerButton::Secondary, true)]);
    frame(vec![pointer(anchor, PointerButton::Secondary, false)]);
    let popup = frame(vec![]);

    let rows = ["Copy", "Paste", "Clear Terminal", "Reset Font Size"].map(|label| {
        let label = crate::i18n::text(&context, label);
        popup
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(text) if text.galley.text() == label => {
                    let rect = Rect::from_min_size(text.pos, text.galley.size());
                    assert!(
                        rect.right() <= shape.clip_rect.right() + 1.0,
                        "{locale}: {label} clipped"
                    );
                    Some(rect)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{label} is missing from the terminal context menu"))
    });
    assert!(
        rows.iter()
            .all(|row| (row.left() - rows[0].left()).abs() < 1.0),
        "menu rows are not aligned: {rows:?}"
    );

    let hit = pos2(rows[3].left() + 145.0, rows[3].center().y);
    frame(vec![Event::PointerMoved(hit)]);
    frame(vec![pointer(hit, PointerButton::Primary, true)]);
    frame(vec![pointer(hit, PointerButton::Primary, false)]);
    assert!(matches!(selected, Some(Action::ResetZoom)));
}
