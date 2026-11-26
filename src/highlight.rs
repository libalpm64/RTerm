use std::{ops::Range, sync::LazyLock};

use eframe::egui::Color32;
use regex::Regex;

use crate::theme::color;

static RULES: LazyLock<Vec<(Regex, Color32)>> = LazyLock::new(|| {
    [
        (r"\b(sudo|rm|dd|mkfs|shutdown|reboot|halt|poweroff)\b", 0xff0000),
        (r"\b(ls|cd|grep|awk|sed|cat|less|more|tail|head|find|ssh|scp|git|docker|npm|cargo|pip|python|gcc|make|apt|yum|dnf|systemctl|journalctl)\b", 0xffff00),
        (r"\b(\d{1,3}(?:\.\d{1,3}){3})\b", 0xff00ff),
        (r"/[a-zA-Z0-9._/-]+|\b[drwx-]{10}\b|\$[a-zA-Z_][a-zA-Z0-9_]*\b", 0x00ffff),
        (r"(?i)\b(error|failed|denied|fatal|invalid)\b", 0xff0000),
    ].into_iter().map(|(regex, hex)| (Regex::new(regex).unwrap(), color(hex))).collect()
});

pub(crate) struct Highlighter {
    enabled: bool,
    keywords: Option<Regex>,
}

impl Highlighter {
    pub(crate) fn new(enabled: bool, keywords: &str) -> Self {
        let terms = keywords
            .split_whitespace()
            .filter(|term| term.len() > 1)
            .take(64)
            .map(regex::escape)
            .collect::<Vec<_>>();
        let keywords = if terms.is_empty() {
            None
        } else {
            Regex::new(&format!(r"(?i)\b(?:{})\b", terms.join("|"))).ok()
        };
        Self { enabled, keywords }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.enabled || self.keywords.is_some()
    }

    // Highlight only cells with the default foreground. Terminal applications
    // retain their own ANSI colors and the parser still receives the raw bytes.
    pub(crate) fn ranges(&self, line: &str) -> Vec<(Range<usize>, Color32)> {
        let mut matches = Vec::new();
        if self.enabled {
            for (pattern, color) in RULES.iter() {
                matches.extend(pattern.find_iter(line).map(|item| (item.range(), *color)));
            }
        }
        if let Some(pattern) = &self.keywords {
            matches.extend(pattern.find_iter(line).map(|item| {
                let keyword = item.as_str().to_lowercase();
                let hex = if ["error", "fail", "fault", "critical", "exception"]
                    .iter()
                    .any(|part| keyword.contains(part))
                {
                    0xff0000
                } else if ["success", "ok", "passed", "done"]
                    .iter()
                    .any(|part| keyword.contains(part))
                {
                    0x00ffff
                } else if ["info", "debug", "trace"]
                    .iter()
                    .any(|part| keyword.contains(part))
                {
                    0x5c5cff
                } else {
                    0xffff00
                };
                (item.range(), color(hex))
            }));
        }
        matches
    }
}
