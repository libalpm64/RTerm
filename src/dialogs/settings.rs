use std::{collections::BTreeMap, ops::RangeInclusive};

use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle};
use eframe::egui::{self, Align, RichText, Sense, Stroke, vec2};

use super::widgets::{combo, input, number_value, toggle};
use crate::{
    app::App,
    storage,
    terminal::TerminalOptions,
    theme::{self, Theme},
};

#[derive(Clone)]
pub(crate) struct Settings {
    values: BTreeMap<&'static str, String>,
    page: usize,
}

pub(super) enum SettingsSection {
    VaultBackups,
    Import,
    Tunnel,
}

const DEFAULTS: &[(&str, &str)] = &[
    ("language", "auto"),
    #[cfg(target_os = "macos")]
    ("interface_scale", "100"),
    ("font_size", "13"),
    ("bottom_font_size", ""),
    ("cursor_style", "block"),
    ("cursor_blink", "1"),
    ("scrollback", "2000"),
    ("bell_enabled", "0"),
    ("copy_on_select", "1"),
    ("free_type", "1"),
    ("line_numbers", "1"),
    ("background_enabled", "0"),
    ("background_path", ""),
    ("background_opacity", "25"),
    ("background_glass", "1"),
    ("semantic_hl", "1"),
    ("highlight_keywords", ""),
    ("ssh_default_port", "22"),
    ("ssh_keepalive", "15"),
    ("ssh_auto_reconnect", "1"),
    ("ssh_compression", "0"),
    ("serial_default_baud", "115200"),
    ("serial_default_bits", "8"),
    ("vault_enabled", "0"),
    ("lock_timeout", "0"),
    ("confirm_exit", "1"),
    ("clear_on_close", "0"),
    ("sftp_concurrent", "3"),
    ("sftp_max_speed", "0"),
    ("sftp_timestamps", "1"),
    ("sftp_resume", "1"),
    ("sftp_show_hidden", "0"),
    ("ssh_agent", "1"),
    ("ai_access", "disabled"),
    ("ai_auto_open", "0"),
    ("ai_upload", "0"),
    ("ai_interactive", "0"),
    ("ai_command_timeout", "60"),
    ("ai_audit_vault", "0"),
    ("ai_tunnel_autoconnect", "0"),
];

#[cfg(target_os = "macos")]
const INTERFACE_SCALES: &[(&str, &str)] = &[
    ("100", "100%"),
    ("110", "110%"),
    ("125", "125%"),
    ("150", "150%"),
];

impl Settings {
    pub(crate) fn open_ai_page(&mut self) {
        self.page = 6;
    }

    pub(super) fn open_security_page(&mut self) {
        self.page = 3;
    }
    pub(crate) fn load() -> Self {
        Self::from_store(storage::load_setting)
    }

    fn from_store(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut values: BTreeMap<&'static str, String> = DEFAULTS
            .iter()
            .copied()
            .map(|(key, default)| (key, lookup(key).unwrap_or_else(|| default.into())))
            .collect();
        values.insert(
            "language",
            crate::i18n::preference(&values["language"]).into(),
        );
        #[cfg(target_os = "macos")]
        if !INTERFACE_SCALES
            .iter()
            .any(|&(scale, _)| scale == values["interface_scale"])
        {
            values.insert("interface_scale", "100".into());
        }
        Self { values, page: 0 }
    }

    #[cfg(any(test, feature = "benchmark"))]
    pub(crate) fn benchmark_defaults() -> Self {
        Self::from_store(|_| None)
    }

    pub(crate) fn get(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }
    pub(crate) fn on(&self, key: &str) -> bool {
        self.get(key) == "1"
    }
    pub(crate) fn background_opacity(&self) -> u8 {
        self.get("background_opacity")
            .parse::<u8>()
            .unwrap_or(25)
            .min(70)
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn interface_scale(&self) -> f32 {
        f32::from(self.get("interface_scale").parse::<u8>().unwrap_or(100)) / 100.0
    }

    pub(crate) fn set(&mut self, key: &'static str, value: String) {
        self.values.insert(key, value);
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let values = self
            .values
            .iter()
            .map(|(key, value)| (*key, value.clone()))
            .collect::<Vec<_>>();
        storage::save_settings(&values)
    }

    pub(crate) fn terminal(&self) -> TerminalOptions {
        let shape = match self.get("cursor_style") {
            "bar" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => CursorShape::Block,
        };
        let font = self
            .get("font_size")
            .parse::<u8>()
            .unwrap_or(13)
            .clamp(8, 32)
            .into();
        let scrollback = self.get("scrollback").parse::<usize>().unwrap_or(2000);
        let scrollback = if scrollback == 0 {
            100_000
        } else {
            scrollback.min(100_000)
        };
        TerminalOptions {
            font,
            cursor: CursorStyle {
                shape,
                blinking: self.on("cursor_blink"),
            },
            scrollback,
            copy_on_select: self.on("copy_on_select"),
            line_numbers: self.on("line_numbers"),
            free_type: self.on("free_type"),
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        t: Theme,
        password: &mut String,
        existing_vault: bool,
        show_section: impl FnOnce(&mut egui::Ui, SettingsSection),
    ) {
        let pages = [
            "Terminal",
            "Appearance",
            "Connection",
            "Security",
            "Import",
            "SFTP",
            "AI",
        ];
        let height = (ui.ctx().content_rect().height() - 160.0).clamp(300.0, 460.0);
        let (body, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let nav = body.with_max_x(body.left() + 144.0);
        theme::band(ui, nav, t.panel, None);
        ui.painter()
            .vline(nav.right(), nav.y_range(), Stroke::new(1.0_f32, t.border));
        for (index, title) in pages.iter().enumerate() {
            let row = egui::Rect::from_min_size(
                nav.min + vec2(0.0, 12.0 + index as f32 * 30.0),
                vec2(nav.width(), 28.0),
            );
            let response = ui.interact(
                row,
                ui.id().with(("settings-category", index)),
                Sense::click(),
            );
            let label = crate::i18n::tr(ui, title);
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    self.page == index,
                    label,
                )
            });
            if response.clicked() {
                self.page = index;
            }
            if self.page == index || response.hovered() {
                theme::band(
                    ui,
                    row,
                    if self.page == index { t.hover } else { t.bar },
                    None,
                );
            }
            if self.page == index {
                ui.painter()
                    .rect_filled(row.with_max_x(row.left() + 2.0), 0.0, t.accent);
            }
            theme::label(
                ui,
                row.shrink2(vec2(12.0, 0.0)),
                label,
                t.chrome_font(12.0),
                if self.page == index { t.text } else { t.muted },
                egui::Align2::LEFT_CENTER,
            );
        }
        let bounds = body.with_min_x(nav.right() + 16.0).shrink2(vec2(0.0, 12.0));
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("settings-content", self.page))
                .max_rect(bounds),
        );
        content.set_clip_rect(bounds);
        content.set_width(bounds.width());
        content.label(
            RichText::new(crate::i18n::tr(&content, pages[self.page]))
                .font(t.chrome_font(16.0))
                .color(t.text),
        );
        content.add_space(8.0);
        content.separator();
        let height = (bounds.bottom() - content.next_widget_position().y).max(0.0);
        settings_scroll(&mut content, t, self.page, height, |ui| {
            ui.add_space(8.0);
            match self.page {
                0 => self.terminal_page(ui, t),
                1 => self.appearance_page(ui, t),
                2 => self.connection_page(ui, t),
                3 => {
                    self.security_page(ui, t, password, existing_vault);
                    show_section(ui, SettingsSection::VaultBackups);
                }
                4 => show_section(ui, SettingsSection::Import),
                5 => self.sftp_page(ui, t),
                _ => {
                    self.ai_page(ui, t);
                    ui.separator();
                    ui.collapsing(
                        crate::i18n::tr(ui, "Advanced · OpenAI Secure MCP Tunnel"),
                        |ui| {
                            if crate::agent::Mode::parse(self.get("ai_access"))
                                == crate::agent::Mode::Disabled
                            {
                                ui.label(
                                    RichText::new(crate::i18n::tr(
                                        ui,
                                        "Enable Agent Access before setting up the tunnel.",
                                    ))
                                    .color(t.muted),
                                );
                                return;
                            }
                            ui.add_enabled_ui(self.on("vault_enabled"), |ui| {
                                self.flag(
                                    ui,
                                    t,
                                    ("Auto-connect tunnel", "Connect when the agent starts"),
                                    "ai_tunnel_autoconnect",
                                );
                            });
                            show_section(ui, SettingsSection::Tunnel);
                        },
                    );
                }
            }
            ui.add_space(8.0);
        });
    }

    fn terminal_page(&mut self, ui: &mut egui::Ui, t: Theme) {
        section(ui, t, "TEXT");
        self.choice(
            ui,
            t,
            ("Font Size", "Terminal font size"),
            "font_size",
            &[
                ("10", "10 px"),
                ("11", "11 px"),
                ("12", "12 px"),
                ("13", "13 px"),
                ("14", "14 px"),
                ("15", "15 px"),
                ("16", "16 px"),
                ("18", "18 px"),
                ("20", "20 px"),
            ],
        );
        self.choice(
            ui,
            t,
            ("Cursor Style", "Terminal cursor shape"),
            "cursor_style",
            &[
                ("block", "Block"),
                ("underline", "Underline"),
                ("bar", "Bar"),
            ],
        );
        self.flag(
            ui,
            t,
            ("Cursor Blink", "Animate the terminal cursor"),
            "cursor_blink",
        );

        ui.add_space(16.0);
        section(ui, t, "BEHAVIOR");
        self.number(ui, t, &SCROLLBACK);
        self.flag(
            ui,
            t,
            ("Bell", "Play sound on terminal bell"),
            "bell_enabled",
        );
        self.flag(
            ui,
            t,
            ("Copy on Select", "Copy selected text to the clipboard"),
            "copy_on_select",
        );
        self.flag(
            ui,
            t,
            (
                "Free Type Mode",
                "Use the mouse to edit echoed shell commands",
            ),
            "free_type",
        );
    }

    fn appearance_page(&mut self, ui: &mut egui::Ui, t: Theme) {
        section(ui, t, "WORKSPACE");
        #[cfg(target_os = "macos")]
        self.choice(
            ui,
            t,
            (
                "Interface Scale",
                "Adjust the size of menus, panels, and text",
            ),
            "interface_scale",
            INTERFACE_SCALES,
        );
        self.choice(
            ui,
            t,
            ("Language", "Interface language"),
            "language",
            crate::i18n::CHOICES,
        );
        self.flag(
            ui,
            t,
            ("Line Numbers", "Show terminal row numbers in the gutter"),
            "line_numbers",
        );
        ui.add_space(12.0);
        section(ui, t, "TERMINAL BACKGROUND");
        self.flag(
            ui,
            t,
            (
                "Background Image",
                "Show an optional PNG, JPEG or HEIF wallpaper",
            ),
            "background_enabled",
        );
        let path = self.get("background_path").to_owned();
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| crate::i18n::tr(ui, "No image selected"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let available = (ui.available_width() - 170.0).max(40.0);
            ui.add_sized(
                vec2(available, 26.0),
                egui::Label::new(RichText::new(name).color(t.muted)).truncate(),
            )
            .on_hover_text(&path);
            if ui.button(crate::i18n::tr(ui, "Import Image…")).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "heif", "heic"])
                    .pick_file()
            {
                self.set("background_path", path.to_string_lossy().into_owned());
            }
            if ui
                .add_enabled(
                    !path.is_empty(),
                    egui::Button::new(crate::i18n::tr(ui, "Clear")),
                )
                .clicked()
            {
                self.set("background_path", String::new());
                self.set("background_enabled", "0".into());
            }
        });
        ui.add_space(5.0);
        self.flag(
            ui,
            t,
            ("Frosted Glass", "Blur the image behind terminal text"),
            "background_glass",
        );
        let mut opacity = self.background_opacity();
        setting_row(
            ui,
            t,
            ("Image Opacity", "Keep the terminal readable over the image"),
            |ui| ui.add(egui::Slider::new(&mut opacity, 0..=70).suffix("%")),
        );
        self.set("background_opacity", opacity.to_string());
        ui.add_space(16.0);
        section(ui, t, "HIGHLIGHTING");
        self.flag(
            ui,
            t,
            (
                "Smart Highlighting",
                "Highlight IP addresses, paths, and commands",
            ),
            "semantic_hl",
        );
        ui.add_space(9.0);
        let label = setting_label(ui, t, ("Custom Keywords", "Separate words with spaces"));
        ui.add_space(7.0);
        input(
            ui,
            t,
            self.values.get_mut("highlight_keywords").unwrap(),
            "ERROR WARN SUCCESS INFO",
            false,
        )
        .labelled_by(label.id);
    }

    fn connection_page(&mut self, ui: &mut egui::Ui, t: Theme) {
        section(ui, t, "SSH DEFAULTS");
        self.flag(
            ui,
            t,
            (
                "Reconnect SSH",
                "Check idle connections and retry disconnected tabs",
            ),
            "ssh_auto_reconnect",
        );
        self.number(ui, t, &SSH_PORT);
        self.number(ui, t, &KEEPALIVE);
        self.flag(
            ui,
            t,
            ("Compression", "Enable SSH compression"),
            "ssh_compression",
        );

        ui.add_space(16.0);
        section(ui, t, "SERIAL DEFAULTS");
        self.choice(
            ui,
            t,
            ("Default Baud Rate", "Speed for new serial connections"),
            "serial_default_baud",
            &BAUD_RATES,
        );
        self.choice(
            ui,
            t,
            ("Default Data Bits", "Bits per character"),
            "serial_default_bits",
            &DATA_BITS,
        );
    }

    fn security_page(
        &mut self,
        ui: &mut egui::Ui,
        t: Theme,
        password: &mut String,
        existing_vault: bool,
    ) {
        section(ui, t, "VAULT ENCRYPTION");
        self.flag(
            ui,
            t,
            ("Enable Vault", "Encrypt saved passwords and keys"),
            "vault_enabled",
        );
        if self.on("vault_enabled") && !existing_vault {
            setting_row(
                ui,
                t,
                ("Vault Password", "Master password for the vault"),
                |ui| input(ui, t, password, "Set master password", true),
            );
        }

        ui.add_space(16.0);
        section(ui, t, "SESSION SECURITY");
        self.number(ui, t, &LOCK_TIMEOUT);
        self.flag(
            ui,
            t,
            ("Confirm Exit", "Ask before closing with active sessions"),
            "confirm_exit",
        );
        self.flag(
            ui,
            t,
            ("Clear History on Close", "Remove command history on exit"),
            "clear_on_close",
        );
    }

    fn ai_page(&mut self, ui: &mut egui::Ui, t: Theme) {
        self.choice(
            ui,
            t,
            ("Agent Access", "Command and upload approvals"),
            "ai_access",
            &[
                ("disabled", "Disabled"),
                ("ask", "Ask permission"),
                ("unrestricted", "Unrestricted"),
            ],
        );
        if self.get("ai_access") == "unrestricted" {
            ui.colored_label(
                t.yellow,
                crate::i18n::tr(ui, "Unrestricted skips approval for background commands and enabled SFTP uploads; foreground terminal commands still require approval."),
            );
        }
        self.flag(
            ui,
            t,
            ("Auto Open Sessions", "Skip approval for saved sessions"),
            "ai_auto_open",
        );
        self.flag(
            ui,
            t,
            ("Allow AI File Uploads", "SFTP follows Agent Access"),
            "ai_upload",
        );
        self.flag(
            ui,
            t,
            ("Run AI Commands in Terminal", "Share the visible SSH shell"),
            "ai_interactive",
        );
        ui.add_enabled_ui(self.on("vault_enabled"), |ui| {
            self.flag(
                ui,
                t,
                (
                    "Log AI Commands to Vault",
                    "Keep up to 250 commands and outcomes",
                ),
                "ai_audit_vault",
            );
        });
        if !self.on("vault_enabled") {
            ui.label(crate::i18n::tr(ui, "Command logging requires the vault."));
        }
        self.choice(
            ui,
            t,
            ("Command Timeout", "Per command"),
            "ai_command_timeout",
            &[
                ("15", "15 seconds"),
                ("30", "30 seconds"),
                ("60", "60 seconds"),
                ("120", "120 seconds"),
                ("300", "5 minutes"),
                ("600", "10 minutes"),
                ("1800", "30 minutes"),
                ("3600", "1 hour"),
            ],
        );
        ui.hyperlink_to(
            crate::i18n::tr(ui, "AI access guide"),
            "https://github.com/libalpm64/RTerm#ai-agent-interface-local-cli",
        );
    }

    fn sftp_page(&mut self, ui: &mut egui::Ui, t: Theme) {
        section(ui, t, "FILE BROWSER");
        self.flag(
            ui,
            t,
            (
                "Show hidden files",
                "Show files and folders starting with a dot",
            ),
            "sftp_show_hidden",
        );
        ui.add_space(16.0);
        section(ui, t, "TRANSFER SETTINGS");
        self.number(ui, t, &CONCURRENT);
        self.number(ui, t, &MAX_SPEED);
        self.flag(
            ui,
            t,
            (
                "Preserve Timestamps",
                "Keep the original file modification time",
            ),
            "sftp_timestamps",
        );
        self.flag(
            ui,
            t,
            (
                "Resume Transfers",
                "Keep checkpoints for single-file transfers",
            ),
            "sftp_resume",
        );
    }

    fn choice(
        &mut self,
        ui: &mut egui::Ui,
        t: Theme,
        label: (&str, &str),
        key: &'static str,
        choices: &[(&str, &str)],
    ) {
        setting_row(ui, t, label, |ui| {
            combo(ui, key, self.values.get_mut(key).unwrap(), choices)
        });
    }

    fn number(&mut self, ui: &mut egui::Ui, t: Theme, setting: &NumericSetting) {
        setting_row(ui, t, setting.label, |ui| {
            input(ui, t, self.values.get_mut(setting.key).unwrap(), "", false)
        });
    }

    fn flag(&mut self, ui: &mut egui::Ui, t: Theme, label: (&str, &str), key: &'static str) {
        let previous = self.on(key);
        let mut value = previous;
        setting_row(ui, t, label, |ui| {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                toggle(ui, label.0, &mut value, t)
            })
            .inner
        });
        if value != previous {
            self.set(key, if value { "1" } else { "0" }.into());
        }
    }

    fn normalize_numbers(&mut self, previous: &Self) -> Result<(), String> {
        for setting in [
            &SCROLLBACK,
            &SSH_PORT,
            &KEEPALIVE,
            &LOCK_TIMEOUT,
            &CONCURRENT,
            &MAX_SPEED,
        ] {
            let text = self.get(setting.key);
            if text != previous.get(setting.key) {
                let value = number_value(setting.label.0, text, setting.range.clone())?;
                self.set(setting.key, value.to_string());
            }
        }
        Ok(())
    }
}

impl App {
    pub(super) fn save_settings_dialog(
        &mut self,
        mut settings: Settings,
        password: &str,
        tunnel: &super::tunnel::Draft,
        context: &egui::Context,
    ) -> Result<(), String> {
        settings.normalize_numbers(&self.settings)?;
        let profile = if settings.on("vault_enabled") {
            if crate::agent::Mode::parse(settings.get("ai_access")) == crate::agent::Mode::Disabled
            {
                self.tunnel_profile.clone()
            } else {
                tunnel.profile(self.tunnel_profile.as_ref())?
            }
        } else {
            settings.set("ai_audit_vault", "0".into());
            settings.set("ai_tunnel_autoconnect", "0".into());
            None
        };
        if settings.values == self.settings.values
            && profile == self.tunnel_profile
            && (!settings.on("vault_enabled") || self.vault_password.is_some())
        {
            return Ok(());
        }
        let wallpaper = crate::background::Wallpaper::from_settings(
            context,
            &settings,
            self.wallpaper.as_ref(),
        )?;
        if settings.on("vault_enabled") {
            if self.vault_password.is_none() {
                if storage::vault_exists() {
                    self.settings = settings;
                    self.wallpaper = wallpaper;
                    self.apply_settings(context);
                    self.lock();
                    return Ok(());
                }
                if password.is_empty() {
                    return Err("Enter a master password to enable the vault".into());
                }
                self.create_vault(password)?;
                // Remove the previous unencrypted profile list after the vault is committed.
                storage::remove_profiles()?;
            }
            self.save_tunnel_profile(profile)?;
        } else if self.settings.on("vault_enabled") {
            storage::save_profiles(&self.profiles)?;
            self.stop_agent();
            self.forget_unlocked_vault();
        }
        self.settings = settings;
        self.wallpaper = wallpaper;
        self.apply_settings(context);
        Ok(())
    }
}

// Keep the scroll track outside the controls, rather than floating above them.
// Only the scrollbar inherits the subdued colors; widgets keep their theme.
fn settings_scroll<R>(
    ui: &mut egui::Ui,
    t: Theme,
    page: usize,
    height: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    let widget_style = ui.style().clone();
    let scroll = &mut ui.spacing_mut().scroll;
    *scroll = egui::style::ScrollStyle::solid();
    scroll.bar_width = 6.0;
    scroll.bar_inner_margin = 10.0;
    scroll.bar_outer_margin = 2.0;
    scroll.handle_min_length = 24.0;
    ui.visuals_mut().extreme_bg_color = t.bar;
    ui.visuals_mut().widgets.inactive.bg_fill = t.outline;
    ui.visuals_mut().widgets.hovered.bg_fill = t.dim;
    ui.visuals_mut().widgets.active.bg_fill = t.muted;
    let output = egui::ScrollArea::vertical()
        .id_salt(("settings-scroll", page))
        .max_height(height)
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_style(widget_style.clone());
            ui.set_min_width(ui.available_width());
            add(ui)
        });
    ui.set_style(widget_style);
    output
}

fn setting_row(
    ui: &mut egui::Ui,
    t: Theme,
    label: (&str, &str),
    field: impl FnOnce(&mut egui::Ui) -> egui::Response,
) {
    let width = ui.available_width();
    let control_width = 144.0_f32.min(width * 0.48);
    let caption_width = (width - control_width - 12.0).max(1.0);
    let (translated_title, translated_details) =
        (crate::i18n::tr(ui, label.0), crate::i18n::tr(ui, label.1));
    // Measure the exact wrapped text using the same fonts as the labels.
    // The row grows instead of clipping a second line at a fixed height.
    let (title, details) = ui.fonts_mut(|fonts| {
        (
            fonts.layout(
                translated_title.into(),
                t.chrome_font(12.0),
                t.text,
                caption_width,
            ),
            fonts.layout(
                translated_details.into(),
                t.chrome_font(10.0),
                t.muted,
                caption_width,
            ),
        )
    });
    let height =
        (title.size().y + details.size().y + 2.0 + 2.0 * ui.spacing().item_spacing.y + 10.0)
            .max(44.0);
    let (row, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let control = egui::Rect::from_center_size(
        egui::pos2(row.right() - control_width / 2.0, row.center().y),
        vec2(control_width, 28.0),
    );
    let caption = row
        .with_max_x(control.left() - 12.0)
        .with_min_y(row.top() + 4.0);
    let mut labels = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("setting-label", label.0))
            .max_rect(caption),
    );
    labels.set_clip_rect(caption.intersect(ui.clip_rect()));
    labels.set_width(caption_width);
    let label_response = labels.add(egui::Label::new(title));
    labels.add_space(2.0);
    labels.add(egui::Label::new(details));
    let mut field_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("setting-control", label.0))
            .max_rect(control),
    );
    field_ui.set_clip_rect(control.intersect(ui.clip_rect()));
    field(&mut field_ui).labelled_by(label_response.id);
}

fn setting_label(ui: &mut egui::Ui, t: Theme, (title, details): (&str, &str)) -> egui::Response {
    let label = ui.label(RichText::new(crate::i18n::tr(ui, title)).size(12.0));
    ui.add_space(2.0);
    ui.label(
        RichText::new(crate::i18n::tr(ui, details))
            .size(10.0)
            .color(t.muted),
    );
    label
}

fn section(ui: &mut egui::Ui, t: Theme, label: &str) {
    ui.label(
        RichText::new(crate::i18n::tr(ui, label))
            .font(t.chrome_font(11.0))
            .color(t.blue),
    );
    ui.add_space(6.0);
}

struct NumericSetting {
    key: &'static str,
    label: (&'static str, &'static str),
    range: RangeInclusive<u64>,
}

const SCROLLBACK: NumericSetting = NumericSetting {
    key: "scrollback",
    label: (
        "Scrollback",
        "Lines in the buffer (0 uses the safe maximum)",
    ),
    range: 0..=100000,
};
const SSH_PORT: NumericSetting = NumericSetting {
    key: "ssh_default_port",
    label: ("Default Port", "Port for new SSH connections"),
    range: 1..=65535,
};
const KEEPALIVE: NumericSetting = NumericSetting {
    key: "ssh_keepalive",
    label: ("Keep Alive", "Interval in seconds (0 turns it off)"),
    range: 0..=3600,
};
const LOCK_TIMEOUT: NumericSetting = NumericSetting {
    key: "lock_timeout",
    label: ("Lock Timeout", "Minutes of inactivity before locking"),
    range: 0..=1440,
};
const CONCURRENT: NumericSetting = NumericSetting {
    key: "sftp_concurrent",
    label: ("Concurrent Downloads", "Parallel workers for downloads"),
    range: 1..=6,
};
const MAX_SPEED: NumericSetting = NumericSetting {
    key: "sftp_max_speed",
    label: ("Max Speed", "Limit in KB/s (0 is unlimited)"),
    range: 0..=1000000,
};

pub(super) const BAUD_RATES: [(&str, &str); 9] = [
    ("1200", "1200"),
    ("2400", "2400"),
    ("4800", "4800"),
    ("9600", "9600"),
    ("19200", "19200"),
    ("38400", "38400"),
    ("57600", "57600"),
    ("115200", "115200"),
    ("230400", "230400"),
];
pub(super) const DATA_BITS: [(&str, &str); 4] = [("5", "5"), ("6", "6"), ("7", "7"), ("8", "8")];

#[cfg(test)]
#[path = "../../tests/unit/dialogs/settings.rs"]
mod tests;
