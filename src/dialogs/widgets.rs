use std::ops::RangeInclusive;

use eframe::egui::{self, Align, RichText, Sense, Stroke, pos2, vec2};

use crate::theme::{self, Theme};

pub(super) const CONTROL_HEIGHT: f32 = 26.0;
pub(super) const DIALOG_INSET: i8 = 14;

pub(super) fn form_field(
    ui: &mut egui::Ui,
    t: Theme,
    name: &str,
    value: &mut String,
    hint: &str,
    password: bool,
) {
    let label = field_label(ui, t, name);
    input(ui, t, value, hint, password).labelled_by(label.id);
}

pub(super) fn field_label(ui: &mut egui::Ui, t: Theme, name: &str) -> egui::Response {
    let label = ui.label(
        RichText::new(crate::i18n::tr(ui, name))
            .size(11.0)
            .color(t.muted),
    );
    ui.add_space(5.0);
    label
}

pub(super) fn input(
    ui: &mut egui::Ui,
    t: Theme,
    value: &mut String,
    hint: &str,
    password: bool,
) -> egui::Response {
    let font = t.chrome_font(12.0);
    ui.add(
        egui::TextEdit::singleline(value)
            .font(font)
            .margin(egui::Margin::symmetric(6, 3))
            .min_size(vec2(0.0, CONTROL_HEIGHT))
            .vertical_align(Align::Center)
            .hint_text(RichText::new(crate::i18n::tr(ui, hint)).color(t.muted))
            .desired_width(f32::INFINITY)
            .password(password),
    )
}

pub(super) fn select<R>(
    ui: &mut egui::Ui,
    id: &str,
    text: &str,
    choices: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
        ui.spacing_mut().button_padding = vec2(6.0, 3.0);
        ui.spacing_mut().icon_width = 12.0;
        ui.visuals_mut().widgets.open.bg_stroke = ui.visuals().selection.stroke;
        egui::ComboBox::from_id_salt(id)
            .width(ui.available_width())
            .selected_text(text)
            .truncate()
            .icon(|ui, rect, visuals, _open| {
                let painter = ui.painter();
                let center = rect.center();
                painter.line_segment(
                    [center + vec2(-3.5, -1.5), center + vec2(0.0, 2.0)],
                    Stroke::new(1.2_f32, visuals.fg_stroke.color),
                );
                painter.line_segment(
                    [center + vec2(0.0, 2.0), center + vec2(3.5, -1.5)],
                    Stroke::new(1.2_f32, visuals.fg_stroke.color),
                );
            })
            .show_ui(ui, choices)
    })
    .inner
}

pub(super) fn combo(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut String,
    choices: &[(&str, &str)],
) -> egui::Response {
    let label = choices
        .iter()
        .find(|(key, _)| key == value)
        .map_or(value.as_str(), |(key, label)| {
            if id == "language" {
                crate::i18n::choice_label(ui, key, label)
            } else {
                crate::i18n::tr(ui, label)
            }
        })
        .to_owned();
    select(ui, id, &label, |ui| {
        for &(key, label) in choices {
            let label = if id == "language" {
                crate::i18n::choice_label(ui, key, label)
            } else {
                crate::i18n::tr(ui, label)
            };
            if ui.selectable_label(key == value, label).clicked() {
                *value = key.into();
            }
        }
    })
    .response
}

pub(super) fn number_value(
    label: &str,
    input: &str,
    range: RangeInclusive<u64>,
) -> Result<u64, String> {
    input
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| range.contains(value))
        .ok_or_else(|| {
            format!(
                "{label} must be a whole number from {} to {}.",
                range.start(),
                range.end()
            )
        })
}

pub(super) fn dialog_tabs(
    ui: &mut egui::Ui,
    id: &str,
    labels: &[&str],
    selected: usize,
    t: Theme,
) -> usize {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let band = rect.expand2(vec2(f32::from(DIALOG_INSET), 0.0));
    theme::band(ui, band, t.bar, Some(t.border));
    let font = t.chrome_font(12.0);
    let mut x = band.left() + 8.0;
    let mut active = selected;
    let responses: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let title = crate::i18n::tr(ui, label);
            let width = theme::text_width(ui, title, font.clone());
            let tab =
                egui::Rect::from_min_size(pos2(x, rect.top()), vec2(width + 30.0, rect.height()));
            x = tab.right();
            let response = ui.interact(tab, ui.id().with((id, index)), Sense::click());
            if response.clicked() {
                active = index;
            }
            response
        })
        .collect();
    for (index, (response, label)) in responses.iter().zip(labels).enumerate() {
        theme::paint_tab(
            ui,
            response,
            crate::i18n::tr(ui, label),
            index == active,
            font.clone(),
            t,
        );
    }
    active
}

pub(super) fn body_scroll(ui: &mut egui::Ui, id: impl std::hash::Hash) -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height((ui.ctx().content_rect().height() * 0.8 - 156.0).max(120.0))
}

pub(super) fn toggle(ui: &mut egui::Ui, label: &str, value: &mut bool, t: Theme) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            *value,
            crate::i18n::tr(ui, label),
        )
    });
    let background = if *value {
        t.accent
    } else if response.hovered() {
        t.outline
    } else {
        t.hover
    };
    ui.painter().rect_filled(rect, 0.0, background);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0_f32, t.outline),
        egui::StrokeKind::Inside,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            0.0,
            Stroke::new(1.0_f32, t.accent),
            egui::StrokeKind::Inside,
        );
    }
    if *value {
        theme::icon(
            ui,
            rect.shrink(2.0),
            theme::Icon::Check,
            egui::Color32::WHITE,
        );
    }
    response
}
