use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Rect, Response, Sense, Stroke, pos2, vec2,
};

#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub bg: Color32,
    pub panel: Color32,
    pub bar: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub green: Color32,
    pub red: Color32,
    pub yellow: Color32,
    pub blue: Color32,
    pub purple: Color32,
}

pub(crate) const fn color(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Theme {
    pub(crate) fn dark() -> Self {
        let colors = [
            0x000000, 0x000000, 0x101010, 0x161616, 0x282828, 0x404040, 0xc8c8c8, 0xa0a0a0,
            0x686868, 0x007acc, 0x55c72f, 0xff008f, 0xdfcc70, 0x2b91af, 0xa78bfa,
        ]
        .map(color);
        let [
            bg,
            panel,
            bar,
            hover,
            border,
            outline,
            text,
            muted,
            dim,
            accent,
            green,
            red,
            yellow,
            blue,
            purple,
        ] = colors;
        Self {
            bg,
            panel,
            bar,
            hover,
            border,
            outline,
            text,
            muted,
            dim,
            accent,
            green,
            red,
            yellow,
            blue,
            purple,
        }
    }

    pub(crate) fn apply(self, context: &egui::Context) {
        #[cfg(target_os = "linux")]
        {
            context.set_theme(egui::Theme::Dark);
            context.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }
        let mut style = egui::Style {
            animation_time: 0.075,
            ..Default::default()
        };
        style
            .text_styles
            .insert(egui::TextStyle::Body, self.chrome_font(12.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, self.chrome_font(12.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, self.chrome_font(11.0));
        style.spacing.item_spacing = vec2(4.0, 3.0);
        style.spacing.button_padding = vec2(5.0, 2.0);
        style.spacing.menu_margin = egui::Margin::same(0);
        style.spacing.scroll.bar_width = 8.0;
        style.visuals.override_text_color = Some(self.text);
        style.visuals.window_fill = self.panel;
        style.visuals.panel_fill = self.bg;
        style.visuals.window_stroke = Stroke::new(1.0_f32, self.outline);
        style.visuals.faint_bg_color = self.bar;
        style.visuals.extreme_bg_color = self.bg;
        style.visuals.text_edit_bg_color = Some(self.bg);
        style.visuals.hyperlink_color = self.accent;
        style.visuals.error_fg_color = self.red;
        style.visuals.selection.bg_fill = self.accent.gamma_multiply(0.3);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, self.accent);
        for (widgets, fill) in [
            (&mut style.visuals.widgets.inactive, self.bar),
            (&mut style.visuals.widgets.hovered, self.hover),
            (&mut style.visuals.widgets.active, self.hover),
            (&mut style.visuals.widgets.open, self.bar),
        ] {
            widgets.bg_fill = fill;
            widgets.weak_bg_fill = fill;
            widgets.bg_stroke = Stroke::new(1.0_f32, self.border);
            widgets.fg_stroke = Stroke::new(1.0_f32, self.text);
            widgets.corner_radius = 0.into();
            widgets.expansion = 0.0;
        }
        style.visuals.window_corner_radius = 0.into();
        style.visuals.menu_corner_radius = 0.into();
        style.visuals.popup_shadow = egui::Shadow::NONE;
        style.visuals.widgets.noninteractive.corner_radius = 0.into();
        context.set_style(style);
    }

    pub(crate) fn chrome_font(self, size: f32) -> FontId {
        FontId::monospace(size)
    }
}

pub(crate) fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("Inter", include_bytes!("../assets/inter.ttf").as_slice()),
        (
            "Inter Semibold",
            include_bytes!("../assets/inter-semibold.ttf").as_slice(),
        ),
        (
            "JetBrains Mono",
            include_bytes!("../assets/jetbrains-mono.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), egui::FontData::from_static(bytes).into());
    }
    let proportional = fonts.families.get_mut(&FontFamily::Proportional).unwrap();
    proportional.insert(0, "Inter".into());
    let mut semibold = proportional.clone();
    semibold.insert(0, "Inter Semibold".into());
    fonts
        .families
        .insert(FontFamily::Name("Semibold".into()), semibold);
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .unwrap()
        .insert(0, "JetBrains Mono".into());
    let locale = crate::i18n::current(context);
    // Load every installed CJK script, irrespective of the UI language:
    // the language picker and terminal input/output both need those glyphs.
    // Prefer the selected locale's glyph forms when fonts overlap. All faces
    // borrow the deduplicated process-lifetime font collections from i18n.
    static SCRIPT_FONTS: std::sync::LazyLock<Vec<(&'static str, std::sync::Arc<egui::FontData>)>> =
        std::sync::LazyLock::new(|| {
            crate::i18n::SCRIPT_LOCALES
                .iter()
                .filter_map(|&code| {
                    crate::i18n::system_font(code).map(|font| (code, std::sync::Arc::new(font)))
                })
                .collect()
        });
    let ordered = SCRIPT_FONTS
        .iter()
        .filter(|(code, _)| *code == locale)
        .chain(SCRIPT_FONTS.iter().filter(|(code, _)| *code != locale));
    for (code, fallback) in ordered {
        let name = format!("System script {code}");
        fonts.font_data.insert(name.clone(), fallback.clone());
        for family in fonts.families.values_mut() {
            family.push(name.clone());
        }
    }
    if crate::i18n::SCRIPT_LOCALES.contains(&locale)
        && !SCRIPT_FONTS.iter().any(|(code, _)| *code == locale)
    {
        // Do not strand a user in an unreadable interface; keep the saved
        // preference and retry the installed font on the next startup.
        crate::i18n::set(context, "en");
    }
    context.set_fonts(fonts);
}

pub(crate) fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Semibold".into()))
}

pub(crate) fn text_width(ui: &mut egui::Ui, text: &str, font: FontId) -> f32 {
    ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.into(), font, Color32::WHITE)
            .size()
            .x
    })
}

pub(crate) fn band(ui: &egui::Ui, rect: Rect, background: Color32, border: Option<Color32>) {
    ui.painter().rect_filled(rect, 0.0, background);
    if let Some(border) = border {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, border),
        );
    }
}

// Bound the scroll viewport to the dock, even when its contents are much taller.
// Hidden scrollbars do not disable wheel or touchpad scrolling.
pub(crate) fn compact_list(height: f32) -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .max_height(height.max(0.0))
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
}

pub(crate) fn label(
    ui: &egui::Ui,
    rect: Rect,
    text: &str,
    font: FontId,
    color: Color32,
    align: Align2,
) {
    let painter = ui.painter().with_clip_rect(rect);
    let galley = painter.layout(text.into(), font, color, rect.width());
    let galley = if galley.rows.len() > 1 {
        let mut job = (*galley.job).clone();
        job.wrap.max_rows = 1;
        painter.layout_job(job)
    } else {
        galley
    };
    painter.galley(
        align.align_size_within_rect(galley.size(), rect).min,
        galley,
        color,
    );
}

pub(crate) fn paint_tab(
    ui: &egui::Ui,
    response: &Response,
    title: &str,
    selected: bool,
    font: FontId,
    t: Theme,
) {
    let rect = response.rect;
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            title,
        )
    });
    // The strip owns its background. Hover changes the label color while focus stays inside the tab.
    if response.has_focus() && !selected && !response.hovered() {
        ui.painter().rect_stroke(
            rect.shrink(3.0),
            3.0,
            Stroke::new(1.0_f32, t.accent),
            egui::StrokeKind::Inside,
        );
    }
    let text = if selected {
        t.accent
    } else if response.hovered() {
        t.text
    } else {
        t.muted
    };
    label(
        ui,
        rect.with_max_y(rect.bottom() - 2.0),
        title,
        font,
        text,
        Align2::CENTER_CENTER,
    );
    if selected {
        ui.painter()
            .rect_filled(rect.with_min_y(rect.bottom() - 2.0), 0.0, t.accent);
    }
}

pub(crate) fn button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: impl std::hash::Hash,
    text: &str,
    font: FontId,
    foreground: Color32,
    theme: Theme,
) -> Response {
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.hover);
    }
    label(
        ui,
        rect,
        crate::i18n::tr(ui, text),
        font,
        if response.hovered() {
            theme.text
        } else {
            foreground
        },
        Align2::CENTER_CENTER,
    );
    response
}

#[derive(Clone, Copy)]
pub(crate) enum Icon {
    Add,
    Close,
    Refresh,
    Up,
    Folder,
    FolderAdd,
    File,
    Play,
    Zoom,
    Menu,
    Trash,
    Key,
    Terminal,
    Server,
    Network,
    Plug,
    Expand,
    Collapse,
    Filter,
    Upload,
    Edit,
    Eye,
    Check,
}

pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    glyph: Icon,
    t: Theme,
) -> Response {
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover);
    }
    icon(
        ui,
        Rect::from_center_size(rect.center(), vec2(14.0, 14.0)),
        glyph,
        if response.hovered() { t.text } else { t.dim },
    );
    response.on_hover_text(crate::i18n::tr(ui, id))
}

pub(crate) fn icon(ui: &egui::Ui, rect: Rect, glyph: Icon, color: Color32) {
    let painter = ui.painter();
    let point = |(x, y)| {
        pos2(
            rect.left() + rect.width() * x / 24.0,
            rect.top() + rect.height() * y / 24.0,
        )
    };
    let stroke = Stroke::new((rect.width() / 13.0).max(0.8), color);
    let path = |coords: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            coords.iter().copied().map(point).collect(),
            stroke,
        ));
    };
    let circle = |p, r| {
        painter.circle_stroke(point(p), r * rect.width() / 24.0, stroke);
    };
    match glyph {
        Icon::Add => {
            path(&[(12., 5.), (12., 19.)]);
            path(&[(5., 12.), (19., 12.)]);
        }
        Icon::Close => {
            path(&[(6., 6.), (18., 18.)]);
            path(&[(18., 6.), (6., 18.)]);
        }
        Icon::Refresh => {
            path(&[(22., 4.), (22., 10.), (16., 10.)]);
            path(&[(2., 20.), (2., 14.), (8., 14.)]);
            path(&[(4., 9.), (7., 5.), (12., 3.), (17., 5.), (22., 10.)]);
            path(&[(2., 14.), (7., 19.), (12., 21.), (17., 19.), (20., 15.)]);
        }
        Icon::Up => path(&[(5., 15.), (12., 8.), (19., 15.)]),
        Icon::Folder => path(&[
            (2., 20.),
            (2., 4.),
            (9., 4.),
            (12., 7.),
            (22., 7.),
            (22., 20.),
            (2., 20.),
        ]),
        Icon::FolderAdd => {
            path(&[
                (2., 20.),
                (2., 4.),
                (9., 4.),
                (12., 7.),
                (22., 7.),
                (22., 20.),
                (2., 20.),
            ]);
            path(&[(12., 10.), (12., 17.)]);
            path(&[(8.5, 13.5), (15.5, 13.5)]);
        }
        Icon::File => {
            path(&[
                (5., 22.),
                (5., 2.),
                (14., 2.),
                (20., 8.),
                (20., 22.),
                (5., 22.),
            ]);
            path(&[(14., 2.), (14., 8.), (20., 8.)]);
        }
        Icon::Play => path(&[(5., 3.), (19., 12.), (5., 21.), (5., 3.)]),
        Icon::Zoom => {
            circle((10., 10.), 7.);
            path(&[(15., 15.), (22., 22.)]);
            path(&[(6., 10.), (14., 10.)]);
            path(&[(10., 6.), (10., 14.)]);
        }
        Icon::Menu => {
            for y in [6., 12., 18.] {
                path(&[(3., y), (21., y)]);
            }
        }
        Icon::Trash => {
            path(&[(3., 6.), (21., 6.)]);
            path(&[(5., 6.), (6., 21.), (18., 21.), (19., 6.)]);
            path(&[(9., 6.), (9., 3.), (15., 3.), (15., 6.)]);
        }
        Icon::Key => {
            circle((8., 8.), 5.);
            path(&[(12., 12.), (21., 21.), (23., 19.)]);
            path(&[(17., 17.), (20., 14.)]);
        }
        Icon::Terminal => {
            path(&[(4., 5.), (10., 11.), (4., 17.)]);
            path(&[(12., 19.), (20., 19.)]);
        }
        Icon::Server => {
            for y in [3., 13.] {
                path(&[(3., y), (21., y), (21., y + 8.), (3., y + 8.), (3., y)]);
                circle((7., y + 4.), 0.8);
            }
        }
        Icon::Network => {
            path(&[(12., 8.), (12., 12.), (5., 12.), (5., 16.)]);
            path(&[(12., 12.), (19., 12.), (19., 16.)]);
            for (x, y) in [(12., 4.), (5., 20.), (19., 20.)] {
                circle((x, y), 3.);
            }
        }
        Icon::Plug => {
            path(&[(8., 3.), (8., 8.), (16., 8.), (16., 3.)]);
            path(&[
                (5., 8.),
                (19., 8.),
                (19., 12.),
                (15., 16.),
                (12., 16.),
                (12., 22.),
            ]);
            path(&[(5., 8.), (5., 12.), (9., 16.), (12., 16.)]);
        }
        Icon::Expand => {
            path(&[(3., 9.), (3., 3.), (9., 3.)]);
            path(&[(15., 3.), (21., 3.), (21., 9.)]);
            path(&[(21., 15.), (21., 21.), (15., 21.)]);
            path(&[(9., 21.), (3., 21.), (3., 15.)]);
        }
        Icon::Collapse => {
            path(&[(3., 9.), (9., 9.), (9., 3.)]);
            path(&[(15., 3.), (15., 9.), (21., 9.)]);
            path(&[(21., 15.), (15., 15.), (15., 21.)]);
            path(&[(9., 21.), (9., 15.), (3., 15.)]);
        }
        Icon::Filter => path(&[
            (3., 4.),
            (21., 4.),
            (15., 12.),
            (15., 20.),
            (9., 17.),
            (9., 12.),
            (3., 4.),
        ]),
        Icon::Upload => {
            path(&[(3., 15.), (3., 21.), (21., 21.), (21., 15.)]);
            path(&[(6., 9.), (12., 3.), (18., 9.)]);
            path(&[(12., 3.), (12., 16.)]);
        }
        Icon::Edit => path(&[
            (4., 15.),
            (4., 20.),
            (9., 20.),
            (21., 8.),
            (16., 3.),
            (4., 15.),
        ]),
        Icon::Eye => {
            path(&[
                (1., 12.),
                (6., 7.),
                (12., 5.),
                (18., 7.),
                (23., 12.),
                (18., 17.),
                (12., 19.),
                (6., 17.),
                (1., 12.),
            ]);
            circle((12., 12.), 3.);
        }
        Icon::Check => path(&[(4., 12.), (10., 18.), (21., 6.)]),
    }
}

#[cfg(test)]
#[path = "../tests/unit/theme.rs"]
mod tests;
