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

#[cfg(target_os = "linux")]
#[test]
fn fontconfig_variable_font_index_selects_collection_face() {
    let path = "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc";
    assert_eq!(
        parse_fontconfig_face(&format!("{path}\n262147\n")),
        Some((std::path::PathBuf::from(path), 3))
    );
    assert!(parse_fontconfig_face("missing index").is_none());
}

#[test]
fn script_fonts_or_readable_english_fallback() {
    for &code in SCRIPT_LOCALES {
        let ctx = egui::Context::default();
        set(&ctx, code);
        crate::theme::install_fonts(&ctx);
        let active = current(&ctx);
        assert!(active == code || active == "en");
        #[cfg(target_os = "linux")]
        if let Some((path, index)) = linux_fontconfig_face(code)
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(face) = ab_glyph::FontRef::try_from_slice_and_index(&bytes, index)
            && linux_font_has_script(&face, code)
        {
            assert_eq!(
                active, code,
                "installed Fontconfig font must activate {code}"
            );
        }
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

#[test]
fn script_font_collections_are_borrowed_and_shared_across_faces() {
    use std::borrow::Cow;
    let mut distinct = BTreeMap::new();
    for (&code, (path, index)) in &SYSTEM_FONTS.faces {
        let font = system_font(code).expect("accepted face");
        assert_eq!(font.index, *index);
        let Cow::Borrowed(bytes) = font.font else {
            panic!("system fonts must not make egui clone their backing data");
        };
        assert!(ab_glyph::FontRef::try_from_slice_and_index(bytes, *index).is_ok());
        let again = system_font(code).unwrap();
        assert_eq!(bytes.as_ptr(), again.font.as_ptr());
        assert_eq!(bytes.as_ptr(), SYSTEM_FONTS.collections[path].as_ptr());
        if let Some(previous) = distinct.insert(path, bytes.as_ptr()) {
            assert_eq!(
                previous,
                bytes.as_ptr(),
                "faces in the same file must share bytes"
            );
        }
    }
    assert_eq!(distinct.len(), SYSTEM_FONTS.collections.len());
    assert!(distinct.len() <= SCRIPT_LOCALES.len());
    assert!(system_font("unknown-script").is_none());
}

#[test]
fn collection_loader_reuses_canonical_path() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.ttf");
    std::fs::write(&file, include_bytes!("../../assets/inter.ttf")).unwrap();
    let mut fonts = SystemFonts::default();
    let first = fonts.load_collection(&file).unwrap();
    let pointer = fonts.collections[&first].as_ptr();
    let alias = directory.path().join(".").join("fixture.ttf");
    let second = fonts.load_collection(&alias).unwrap();
    assert_eq!(first, second);
    assert_eq!(fonts.collections.len(), 1);
    assert_eq!(fonts.collections[&second].as_ptr(), pointer);
    assert!(
        fonts
            .load_collection(&directory.path().join("missing.ttf"))
            .is_none()
    );
    #[cfg(unix)]
    {
        let link = directory.path().join("alias.ttf");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(fonts.load_collection(&link).unwrap(), first);
        assert_eq!(fonts.collections.len(), 1);
    }
}

#[test]
fn repeated_locale_switches_keep_shared_font_backing_and_glyph_coverage() {
    let ctx = egui::Context::default();
    for code in ["en", "ja", "ko", "zh-TW", "zh-CN", "en"]
        .into_iter()
        .cycle()
        .take(24)
    {
        set(&ctx, code);
        crate::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                let definitions = fonts.definitions();
                let family = &definitions.families[&egui::FontFamily::Monospace];
                if let Some(expected) = system_font(code) {
                    let first = family
                        .iter()
                        .find(|name| name.starts_with("System script "))
                        .unwrap();
                    assert_eq!(first, &format!("System script {code}"));
                    let face = &definitions.font_data[first];
                    assert_eq!(face.index, expected.index);
                    assert_eq!(face.font.as_ptr(), expected.font.as_ptr());
                }
                for &(lang, label) in CHOICES {
                    if SCRIPT_LOCALES.contains(&lang) && system_font(lang).is_some() {
                        assert!(fonts.has_glyphs(&egui::FontId::monospace(12.0), label));
                    }
                }
            });
        });
    }
}
