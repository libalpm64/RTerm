//! UI-only localization. English source strings are the fallback, never keys.
use std::{collections::BTreeMap, sync::LazyLock};

use eframe::egui;

const ID: &str = "rterm-locale";
type Catalog = BTreeMap<String, String>;

macro_rules! locales {
    ($(($code:literal, $name:literal)),+ $(,)?) => {
        pub(crate) const CHOICES: &[(&str, &str)] = &[
            ("auto", "System default"), $(($code, $name)),+
        ];
        const SOURCES: &[(&str, &str)] = &[
            $(($code, include_str!(concat!("../locales/", $code, ".json")))),+
        ];
    };
}

locales!(
    ("en", "English"),
    ("zh-CN", "简体中文"),
    ("ja", "日本語"),
    ("es", "Español"),
    ("pt-BR", "Português (Brasil)"),
    ("ru", "Русский"),
    ("de", "Deutsch"),
    ("ko", "한국어"),
    ("fr", "Français"),
    ("zh-TW", "繁體中文"),
    ("it", "Italiano"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
);

static CATALOGS: LazyLock<BTreeMap<&'static str, Catalog>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .map(|&(code, source)| {
            (
                code,
                serde_json::from_str(source).expect("embedded translation catalog"),
            )
        })
        .collect()
});

fn match_locale(raw: &str) -> &'static str {
    let tag = raw
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_ascii_lowercase();
    let mut parts = tag.split('-');
    match parts.next().unwrap_or("") {
        "zh" => {
            let parts: Vec<_> = parts.collect();
            if parts.contains(&"hant") {
                "zh-TW"
            } else if parts.contains(&"hans") {
                "zh-CN"
            } else if parts.iter().any(|p| matches!(*p, "tw" | "hk" | "mo")) {
                "zh-TW"
            } else {
                "zh-CN"
            }
        }
        "pt" => "pt-BR",
        base => SOURCES
            .iter()
            .find(|(code, _)| *code == base)
            .map_or("en", |(code, _)| *code),
    }
}

fn system_locale() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::{
            GetUserDefaultLocaleName, GetUserPreferredUILanguages, MUI_LANGUAGE_NAME,
        };
        let mut count = 0;
        let mut length = 0;
        // First query the buffer length, then read the UI language MULTI_SZ.
        if unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut length,
            )
        } != 0
            && (2..=32768).contains(&length)
        {
            let mut languages = vec![0_u16; length as usize];
            if unsafe {
                GetUserPreferredUILanguages(
                    MUI_LANGUAGE_NAME,
                    &mut count,
                    languages.as_mut_ptr(),
                    &mut length,
                )
            } != 0
                && let Some(end) = languages.iter().position(|unit| *unit == 0)
                && end > 0
            {
                return String::from_utf16_lossy(&languages[..end]);
            }
        }
        let mut buffer = [0_u16; 85];
        // The API writes at most buffer.len() UTF-16 code units, including NUL.
        let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
        if len > 1 && len as usize <= buffer.len() {
            return String::from_utf16_lossy(&buffer[..len as usize - 1]);
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        && output.status.success()
    {
        let languages = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = languages
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && *line != "(" && *line != ")")
        {
            return first.trim_end_matches(',').trim_matches('"').to_owned();
        }
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

pub(crate) fn resolve(preference: &str) -> &'static str {
    if preference == "auto" {
        match_locale(&system_locale())
    } else {
        match_locale(preference)
    }
}

pub(crate) fn preference(raw: &str) -> &'static str {
    if raw == "auto" {
        "auto"
    } else {
        match_locale(raw)
    }
}

pub(crate) fn current(context: &egui::Context) -> &'static str {
    context
        .data(|data| data.get_temp::<&'static str>(egui::Id::new(ID)))
        .unwrap_or("en")
}

pub(crate) fn set(context: &egui::Context, code: &'static str) {
    context.data_mut(|data| data.insert_temp(egui::Id::new(ID), code));
}

fn translate<'a>(code: &str, english: &'a str) -> &'a str {
    if code == "en" {
        return english;
    }
    CATALOGS
        .get(code)
        .and_then(|catalog| catalog.get(english))
        .filter(|value| !value.is_empty())
        .map_or(english, String::as_str)
}

pub(crate) fn tr<'a>(ui: &egui::Ui, english: &'a str) -> &'a str {
    text(ui.ctx(), english)
}

pub(crate) fn text<'a>(context: &egui::Context, english: &'a str) -> &'a str {
    translate(current(context), english)
}

pub(crate) const SCRIPT_LOCALES: &[&str] = &["zh-CN", "zh-TW", "ja", "ko"];

pub(crate) fn choice_label<'a>(ui: &mut egui::Ui, code: &str, native: &'a str) -> &'a str {
    let label = tr(ui, native);
    if ui.fonts_mut(|fonts| fonts.has_glyphs(&egui::FontId::monospace(12.0), label)) {
        return label;
    }
    // Keep the selector usable even on systems without a CJK language pack.
    match code {
        "zh-CN" => "Chinese (Simplified)",
        "zh-TW" => "Chinese (Traditional)",
        "ja" => "Japanese",
        "ko" => "Korean",
        "auto" => "System default",
        _ => label,
    }
}

/// Load an installed script font; do not redistribute OS fonts or fetch fonts
/// from the network. The bundled Latin fonts retain first priority.
pub(crate) fn system_font(code: &str) -> Option<egui::FontData> {
    use std::path::PathBuf;
    let mut candidates: Vec<(PathBuf, u32)> = Vec::new();
    #[cfg(windows)]
    {
        let root = PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
        let name = match code {
            "zh-CN" => "msyh.ttc",
            "zh-TW" => "msjh.ttc",
            "ja" => "meiryo.ttc",
            "ko" => "malgun.ttf",
            _ => return None,
        };
        candidates.push((root.join(name), 0));
    }
    #[cfg(target_os = "macos")]
    {
        let (file, index) = match code {
            "zh-CN" | "zh-TW" => ("PingFang.ttc", 0),
            "ja" => ("ヒラギノ角ゴシック W3.ttc", 0),
            "ko" => ("AppleSDGothicNeo.ttc", 0),
            _ => return None,
        };
        candidates.push((PathBuf::from("/System/Library/Fonts").join(file), index));
        candidates.push((
            PathBuf::from("/System/Library/Fonts/Supplemental").join(file),
            index,
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let index = match code {
            "ja" => 0,
            "ko" => 1,
            "zh-CN" => 2,
            "zh-TW" => 3,
            _ => return None,
        };
        for directory in [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/noto-cjk",
            "/usr/share/fonts/google-noto-cjk",
        ] {
            candidates.push((
                PathBuf::from(directory).join("NotoSansCJK-Regular.ttc"),
                index,
            ));
        }
    }
    candidates.into_iter().find_map(|(path, index)| {
        let bytes = std::fs::read(path).ok()?;
        // egui panics on malformed fonts. Validate with its own parser first.
        ab_glyph::FontRef::try_from_slice_and_index(&bytes, index).ok()?;
        let mut font = egui::FontData::from_owned(bytes);
        font.index = index;
        Some(font)
    })
}

/// Substitute only named fields in known UI templates, without parsing user text.
pub(crate) fn format(ui: &egui::Ui, english: &str, fields: &[(&str, &str)]) -> String {
    let template = tr(ui, english);
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((prefix, tail)) = rest.split_once('{') {
        result.push_str(prefix);
        let Some((name, remaining)) = tail.split_once('}') else {
            result.push('{');
            result.push_str(tail);
            return result;
        };
        if let Some((_, value)) = fields.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push('{');
            result.push_str(name);
            result.push('}');
        }
        rest = remaining;
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<&str> {
        assert!(!text.contains(['｛', '｝']), "full-width braces: {text}");
        let mut rest = text;
        let mut fields = Vec::new();
        while let Some((prefix, tail)) = rest.split_once('{') {
            assert!(!prefix.contains('}'), "unmatched brace: {text}");
            let (field, remaining) = tail.split_once('}').expect("unclosed placeholder");
            assert!(field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            fields.push(field);
            rest = remaining;
        }
        assert!(!rest.contains('}'), "unmatched brace: {text}");
        fields.sort_unstable();
        fields
    }

    #[test]
    fn all_catalog_placeholders_match_english() {
        let english = &CATALOGS["en"];
        for (code, catalog) in CATALOGS.iter() {
            for (key, translated) in catalog {
                let original = &english[key];
                assert!(!translated.trim().is_empty(), "{code}: {key}");
                assert_eq!(
                    placeholders(original),
                    placeholders(translated),
                    "{code}: {key}"
                );
                const PROTECTED: &[&str] = &[
                    "ssh",
                    "sftp",
                    "git",
                    "docker",
                    "localhost",
                    "root",
                    "sudo",
                    "terminal",
                    "commit",
                    "push",
                    "pull",
                    "merge",
                ];
                let words = |text: &str, term: &str| {
                    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .filter(|word| word.eq_ignore_ascii_case(term))
                        .count()
                };
                for term in PROTECTED {
                    assert!(
                        words(translated, term) >= words(original, term),
                        "{code}: {key}: missing protected term {term}"
                    );
                }
            }
        }
        for (key, value) in english {
            assert_eq!(key, value);
        }
    }

    #[test]
    fn locale_matching_and_fallback() {
        for (input, expected) in [
            ("zh_Hant_HK", "zh-TW"),
            ("zh_CN.UTF-8", "zh-CN"),
            ("zh-Hans-HK", "zh-CN"),
            ("ja-JP", "ja"),
            ("pt_PT", "pt-BR"),
            ("de_DE.UTF-8", "de"),
            ("he-IL", "en"),
            ("hi-IN", "en"),
            ("ar", "en"),
            ("C", "en"),
            ("uk-UA", "uk"),
        ] {
            assert_eq!(match_locale(input), expected);
        }
        assert_eq!(translate("de", "Settings"), "Einstellungen");
        assert_eq!(translate("de", "New future label"), "New future label");
        assert_eq!(translate("missing", "File"), "File");
    }

    #[test]
    fn locales_are_context_local() {
        let a = egui::Context::default();
        let b = egui::Context::default();
        set(&a, "ja");
        assert_eq!(current(&a), "ja");
        assert_eq!(current(&b), "en");
    }

    #[test]
    fn malformed_placeholders_are_rejected() {
        for invalid in ["｛name｝", "{name", "name}", "{{name}}", "{na me}"] {
            assert!(std::panic::catch_unwind(|| placeholders(invalid)).is_err());
        }
        assert_eq!(placeholders("{name} {} {name}"), ["", "name", "name"]);
    }

    #[test]
    fn inserted_values_are_not_interpreted_as_templates() {
        let ctx = egui::Context::default();
        set(&ctx, "de");
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let source = "Permanently delete “{name}”?";
                assert_eq!(
                    format(ui, source, &[("name", "{count}"), ("count", "42")]),
                    translate("de", source).replace("{name}", "{count}")
                );
            });
        });
    }

    #[test]
    fn selector_uses_native_language_names_in_english() {
        assert_eq!(CHOICES.len(), 15); // System default + 14 languages.
        assert!(!CHOICES.iter().any(|(code, _)| *code == "hi"));
        assert!(CHOICES.contains(&("ja", "日本語")));
        assert!(CHOICES.contains(&("zh-CN", "简体中文")));
        assert!(CHOICES.contains(&("zh-TW", "繁體中文")));
        assert!(CHOICES.contains(&("ko", "한국어")));
        assert!(CHOICES.contains(&("ru", "Русский")));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_eq!(current(ctx), "en");
                for &(code, label) in CHOICES {
                    let displayed = choice_label(ui, code, label);
                    assert!(ui.fonts_mut(|fonts| {
                        fonts.has_glyphs(&egui::FontId::monospace(12.0), displayed)
                    }));
                    if !SCRIPT_LOCALES.contains(&code) || system_font(code).is_some() {
                        assert_eq!(displayed, label, "{code}");
                    }
                }
            });
        });
    }

    #[test]
    fn script_fonts_or_readable_english_fallback() {
        for &code in SCRIPT_LOCALES {
            let ctx = egui::Context::default();
            set(&ctx, code);
            crate::theme::install_fonts(&ctx);
            let active = current(&ctx);
            assert!(active == code || active == "en");
            let _ = ctx.run(Default::default(), |ctx| {
                let label = text(ctx, "Settings");
                ctx.fonts_mut(|fonts| {
                    assert!(
                        fonts.has_glyphs(&egui::FontId::monospace(12.0), label),
                        "{code}: missing settings glyphs"
                    );
                });
            });
            println!("{code}: active language {active}");
        }
    }
}
