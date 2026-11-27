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
        // Fontconfig knows distribution-specific paths, per-user fonts, and
        // variable font collections (e.g. Fedora's NotoSansCJK-VF.ttc).
        if let Some(face) = linux_fontconfig_face(code) {
            candidates.push(face);
        }
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
        let face = ab_glyph::FontRef::try_from_slice_and_index(&bytes, index).ok()?;
        #[cfg(target_os = "linux")]
        if !linux_font_has_script(&face, code) {
            return None;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = face;
        let mut font = egui::FontData::from_owned(bytes);
        font.index = index;
        Some(font)
    })
}

#[cfg(target_os = "linux")]
fn linux_fontconfig_face(code: &str) -> Option<(std::path::PathBuf, u32)> {
    let language = match code {
        "ja" => "ja",
        "ko" => "ko",
        "zh-CN" => "zh-cn",
        "zh-TW" => "zh-tw",
        _ => return None,
    };
    let client = crate::tunnel::find_trusted_executable("fc-match")?;
    let output = std::process::Command::new(client)
        .args([
            "--format",
            "%{file}\n%{index}\n",
            &format!("sans-serif:lang={language}"),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_fontconfig_face(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(target_os = "linux")]
fn parse_fontconfig_face(output: &str) -> Option<(std::path::PathBuf, u32)> {
    let mut lines = output.lines();
    let path = std::path::PathBuf::from(lines.next()?);
    if path.as_os_str().is_empty() {
        return None;
    }
    // Fontconfig encodes the FreeType named-instance index in the upper
    // 16 bits. ab_glyph expects only the collection's face index.
    let index = lines.next()?.parse::<u32>().ok()? & 0xffff;
    Some((path, index))
}

#[cfg(target_os = "linux")]
fn linux_font_has_script(font: &ab_glyph::FontRef<'_>, code: &str) -> bool {
    use ab_glyph::Font;
    // fc-match can return an unrelated fallback even when no font supports
    // the requested language. Check the script before accepting that match.
    let Some((_, native)) = CHOICES.iter().find(|(key, _)| *key == code) else {
        return false;
    };
    native
        .chars()
        .chain(translate(code, "Settings").chars())
        .filter(|ch| !ch.is_whitespace())
        .all(|ch| font.glyph_id(ch).0 != 0)
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
#[path = "../tests/unit/i18n.rs"]
mod tests;
