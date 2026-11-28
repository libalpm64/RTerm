//! The known-hosts viewer is a snapshot, not the authority for SSH trust.
//! Reload on open, explicit refresh, and trust changes. Forgetting keys
//! always compares the reviewed snapshot against the current on-disk store.
use super::*;
use crate::host_keys::{Key, Store, display_host};
use std::{cmp::Ordering, collections::BTreeSet};

struct HostRow {
    target: String,
    host: String,
    port: Option<u16>,
    key: Key,
    algorithm: String,
    fingerprint: String,
    search: String,
}

impl HostRow {
    fn new(target: String, key: Key) -> Self {
        let (name, port) = target
            .strip_prefix('[')
            .and_then(|value| value.rsplit_once("]:"))
            .and_then(|(name, port)| Some((name, Some(port.parse::<u16>().ok()?))))
            .unwrap_or((&target, None));
        let host = display_host(name);
        let algorithm = display_host(&key.algorithm);
        let fingerprint = display_host(&key.fingerprint);
        let endpoint = port.map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
        let search = format!("{target} {host} {endpoint} {algorithm} {fingerprint}").to_lowercase();
        Self {
            target,
            host,
            port,
            key,
            algorithm,
            fingerprint,
            search,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Host,
    Port,
    Algorithm,
    Fingerprint,
}

impl Sort {
    const COLUMNS: [(Self, &'static str); 4] = [
        (Self::Host, "Host"),
        (Self::Port, "Port"),
        (Self::Algorithm, "Key Type"),
        (Self::Fingerprint, "Fingerprint"),
    ];

    fn compare(self, left: &HostRow, right: &HostRow) -> Ordering {
        match self {
            Self::Host => left.host.cmp(&right.host).then(left.port.cmp(&right.port)),
            Self::Port => left.port.cmp(&right.port),
            Self::Algorithm => left.algorithm.cmp(&right.algorithm),
            Self::Fingerprint => left.fingerprint.cmp(&right.fingerprint),
        }
        .then(left.target.cmp(&right.target))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    None,
    Refresh,
    Close,
    Review,
    Cancel,
    Forget,
}

pub(super) struct KnownHosts {
    rows: Vec<HostRow>,
    visible: Vec<usize>,
    // Indices are stable until reload. Selection never includes hidden rows.
    selected: BTreeSet<usize>,
    query: String,
    sort: Sort,
    descending: bool,
    reset_scroll: bool,
    confirming: bool,
    error: Option<String>,
}

impl KnownHosts {
    pub(super) fn new(store: &Store) -> Self {
        let mut viewer = Self {
            rows: Vec::new(),
            visible: Vec::new(),
            selected: BTreeSet::new(),
            query: String::new(),
            sort: Sort::Host,
            descending: false,
            reset_scroll: true,
            confirming: false,
            error: None,
        };
        viewer.reload(store);
        viewer
    }

    pub(super) fn reload(&mut self, store: &Store) {
        self.load(store.list());
    }

    fn load(&mut self, entries: Result<BTreeMap<String, Key>, String>) {
        self.selected.clear();
        self.confirming = false;
        self.error = None;
        self.rows = match entries {
            Ok(entries) => entries
                .into_iter()
                .map(|(host, key)| HostRow::new(host, key))
                .collect(),
            Err(error) => {
                self.error = Some(error);
                Vec::new()
            }
        };
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query
                    .split_whitespace()
                    .all(|word| row.search.contains(word))
            })
            .map(|(index, _)| index)
            .collect();
        self.sort_visible();
    }

    fn sort_visible(&mut self) {
        self.visible.sort_unstable_by(|&left, &right| {
            let order = self.sort.compare(&self.rows[left], &self.rows[right]);
            if self.descending {
                order.reverse()
            } else {
                order
            }
        });
        self.reset_scroll = true;
    }

    fn change_query(&mut self) {
        self.selected.clear();
        self.confirming = false;
        self.rebuild();
    }

    fn change_sort(&mut self, column: Sort) {
        if self.sort == column {
            self.descending = !self.descending;
        } else {
            self.sort = column;
            self.descending = false;
        }
        self.sort_visible();
    }

    pub(super) fn cancel_confirmation(&mut self) {
        self.confirming = false;
    }

    fn expected_keys(&self) -> BTreeMap<String, Key> {
        self.selected
            .iter()
            .map(|&index| {
                let row = &self.rows[index];
                (row.target.clone(), row.key.clone())
            })
            .collect()
    }

    fn forget(&mut self, store: &mut Store) {
        // Keep the exact reviewed host/key pairs, not row numbers in a
        // refreshed store. All changes are made under one storage lock.
        let result = store.forget_many(&self.expected_keys());
        self.reload(store);
        if let Err(error) = result {
            self.error = Some(error);
        }
    }

    /// Returns true when the existing modal should close.
    pub(super) fn show(
        &mut self,
        context: &egui::Context,
        store: &mut Store,
        theme: Theme,
    ) -> bool {
        let max = (context.content_rect().size() - vec2(48.0, 48.0)).max(vec2(1.0, 1.0));
        let response = egui::Modal::new(egui::Id::unique("known-ssh-hosts")).show(context, |ui| {
            egui::Resize::default()
                .id_salt("known-hosts-size")
                .default_size(vec2(760.0, 480.0).min(max))
                .min_size(vec2(500.0, 300.0).min(max))
                .max_size(max)
                .with_stroke(false)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.confirming {
                        self.confirmation(ui, theme)
                    } else {
                        self.content(ui, theme)
                    }
                })
        });
        match response.inner {
            Intent::Refresh => self.reload(store),
            Intent::Close => return true,
            Intent::Review => self.confirming = !self.selected.is_empty(),
            Intent::Cancel => self.confirming = false,
            Intent::Forget if self.confirming => self.forget(store),
            _ => {
                if response.should_close() {
                    if self.confirming {
                        self.confirming = false;
                    } else {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn content(&mut self, ui: &mut egui::Ui, theme: Theme) -> Intent {
        use crate::i18n::{format, tr};

        let mut intent = Intent::None;
        ui.horizontal(|ui| {
            ui.heading(tr(ui, "Known SSH Hosts"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr(ui, "Close")).clicked() {
                    intent = Intent::Close;
                }
            });
        });
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr(ui, "Refresh")).clicked() {
                    intent = Intent::Refresh;
                }
                if ui
                    .add_enabled(!self.query.is_empty(), egui::Button::new(tr(ui, "Clear")))
                    .clicked()
                {
                    self.query.clear();
                    self.change_query();
                }
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text(tr(ui, "Search host, port, key type or fingerprint"))
                            .desired_width(ui.available_width()),
                    )
                    .changed()
                {
                    self.change_query();
                }
            });
        });

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.visible.is_empty(),
                    egui::Button::new(tr(ui, "Select matching")),
                )
                .clicked()
            {
                self.selected = self.visible.iter().copied().collect();
            }
            if ui
                .add_enabled(
                    !self.selected.is_empty(),
                    egui::Button::new(tr(ui, "Clear selection")),
                )
                .clicked()
            {
                self.selected.clear();
            }
            if ui
                .add_enabled(
                    self.selected.len() == 1,
                    egui::Button::new(tr(ui, "Copy fingerprint")),
                )
                .clicked()
                && let Some(&index) = self.selected.first()
            {
                // The displayed value is literal-safe even if the file was
                // manually edited. Valid SSH SHA-256 fingerprints are unchanged.
                ui.ctx().copy_text(self.rows[index].fingerprint.clone());
            }
            if ui
                .add_enabled(
                    !self.selected.is_empty(),
                    egui::Button::new(tr(ui, "Forget selected")),
                )
                .clicked()
            {
                intent = Intent::Review;
            }
        });
        ui.label(format(
            ui,
            "{shown} of {total} trusted hosts · {selected} selected",
            &[
                ("shown", &self.visible.len().to_string()),
                ("total", &self.rows.len().to_string()),
                ("selected", &self.selected.len().to_string()),
            ],
        ));
        if let Some(error) = &self.error {
            ui.colored_label(theme.red, error);
        }
        ui.separator();
        if self.visible.is_empty() {
            if self.error.is_none() {
                ui.label(tr(
                    ui,
                    if self.rows.is_empty() {
                        "No trusted hosts yet."
                    } else {
                        "No trusted hosts match this search."
                    },
                ));
            }
        } else {
            self.table(ui);
        }
        intent
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
    ) -> egui::scroll_area::ScrollAreaOutput<std::ops::Range<usize>> {
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        let height = ui.spacing().interact_size.y.max(24.0);
        let check = height;
        let width = (ui.available_width()
            - ui.spacing().scroll.allocated_width()
            - ui.spacing().item_spacing.x * 4.0
            - check
            - 2.0)
            .max(4.0);
        let widths = [width * 0.32, width * 0.09, width * 0.23, width * 0.36];
        ui.horizontal(|ui| {
            ui.allocate_exact_size(vec2(check, height), Sense::hover());
            for ((column, label), width) in Sort::COLUMNS.into_iter().zip(widths) {
                let label = crate::i18n::tr(ui, label);
                let active = self.sort == column;
                let title = if active {
                    format!("{label} {}", if self.descending { "↓" } else { "↑" })
                } else {
                    label.to_owned()
                };
                if ui
                    .add_sized(
                        [width, height],
                        egui::Button::new(title).truncate().selected(active),
                    )
                    .on_hover_text(label)
                    .clicked()
                {
                    self.change_sort(column);
                }
            }
        });
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("known-hosts-rows")
            .max_height(ui.available_height().max(0.0))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
        if std::mem::take(&mut self.reset_scroll) {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show_rows(ui, height, self.visible.len(), |ui, range| {
            for index in range.clone() {
                let index = self.visible[index];
                let row = &self.rows[index];
                let mut checked = self.selected.contains(&index);
                ui.push_id(&row.target, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized([check, height], egui::Checkbox::without_text(&mut checked));
                        let host = ui
                            .add_sized(
                                [widths[0], height],
                                egui::Label::new(egui::RichText::new(&row.host).strong())
                                    .truncate()
                                    .selectable(false)
                                    .sense(Sense::click()),
                            )
                            .on_hover_text(display_host(&row.target));
                        if host.clicked() {
                            checked = !checked;
                        }
                        let port = row.port.map_or("—".into(), |port| port.to_string());
                        for (text, width) in [
                            egui::RichText::new(port),
                            egui::RichText::new(&row.algorithm),
                            egui::RichText::new(&row.fingerprint).monospace(),
                        ]
                        .into_iter()
                        .zip(widths[1..].iter().copied())
                        {
                            let full = text.text().to_owned();
                            ui.add_sized(
                                [width, height],
                                egui::Label::new(text).truncate().selectable(false),
                            )
                            .on_hover_text(full);
                        }
                    });
                });
                if checked {
                    self.selected.insert(index);
                } else {
                    self.selected.remove(&index);
                }
            }
            range
        })
    }

    fn confirmation(&self, ui: &mut egui::Ui, theme: Theme) -> Intent {
        use crate::i18n::{format, tr};
        ui.heading(format(
            ui,
            "Forget {count} trusted hosts?",
            &[("count", &self.selected.len().to_string())],
        ));
        ui.colored_label(
            theme.yellow,
            tr(
                ui,
                "These hosts will require verification on their next connection.",
            ),
        );
        ui.label(tr(ui, "Existing SSH connections will remain open."));
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("forget-preview")
            .max_height((ui.available_height() - 48.0).max(0.0))
            .show(ui, |ui| {
                for &index in self.selected.iter().take(5) {
                    let row = &self.rows[index];
                    let target = display_host(&row.target);
                    ui.add(egui::Label::new(egui::RichText::new(&target).strong()).truncate())
                        .on_hover_text(target);
                    ui.add(
                        egui::Label::new(egui::RichText::new(&row.fingerprint).monospace())
                            .truncate(),
                    )
                    .on_hover_text(&row.fingerprint);
                }
                if self.selected.len() > 5 {
                    ui.label(format(
                        ui,
                        "And {count} more",
                        &[("count", &(self.selected.len() - 5).to_string())],
                    ));
                }
            });
        ui.separator();
        let mut intent = Intent::None;
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr(ui, "Cancel")).clicked() {
                intent = Intent::Cancel;
            }
            if ui
                .button(egui::RichText::new(tr(ui, "Forget selected")).color(theme.red))
                .clicked()
            {
                intent = Intent::Forget;
            }
        });
        intent
    }
}

#[cfg(test)]
#[path = "../../tests/unit/app/known_hosts.rs"]
mod tests;
