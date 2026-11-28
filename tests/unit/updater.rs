use super::*;
use crate::egui_test::TestContextExt;
use serde_json::{Value, json};

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const SUFFIX: &str = "-linux-amd64.tar.gz";

fn fixture(version: &str) -> Value {
    let name = format!("RTerm-{version}{SUFFIX}");
    json!({
        "tag_name": format!("rterm-{NEW}"),
        "draft": false,
        "prerelease": false,
        "immutable": true,
        "assets": [{
            "name": name,
            "state": "uploaded",
            "size": 3,
            "digest": format!("sha256:{}", sha256(b"abc")),
            "browser_download_url": format!("{REPOSITORY}/releases/download/rterm-{NEW}/{name}"),
        }],
    })
}

fn parse(value: &Value) -> Result<Release, String> {
    select_release(&serde_json::to_vec(value).unwrap(), SUFFIX)
}

#[test]
fn selects_only_verified_immutable_platform_assets() {
    let valid = fixture("2.0.0");
    let release = parse(&valid).unwrap();
    assert_eq!(release.version, "2.0.0");
    assert_eq!(release.revision, NEW);
    assert_eq!(release.size, 3);
    assert_eq!(
        release.digest,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut invalid = valid.clone();
    invalid["immutable"] = json!(false);
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["prerelease"] = json!(true);
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["draft"] = json!(true);
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["tag_name"] = json!("v2.0.0");
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["assets"][0]["digest"] = json!(null);
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["assets"][0]["browser_download_url"] = json!("https://example.com/installer");
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["assets"][0]["size"] = json!(MAX_PACKAGE + 1);
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    invalid["assets"][0]["state"] = json!("new");
    assert!(parse(&invalid).is_err());
    invalid = valid.clone();
    let duplicate = invalid["assets"][0].clone();
    invalid["assets"].as_array_mut().unwrap().push(duplicate);
    assert!(parse(&invalid).is_err());
    assert!(select_release(&serde_json::to_vec(&valid).unwrap(), "-macos-arm64.pkg").is_err());
    for invalid_version in ["2.0", "02.0.0", "2.0.0-beta", "../2.0.0"] {
        assert!(parse(&fixture(invalid_version)).is_err());
    }
}

#[test]
fn updates_by_numeric_version_and_commit_without_downgrading_beta() {
    let release = parse(&fixture("2.0.0")).unwrap();
    let result = assess(release.clone(), "2.0.0", Some(OLD), |from, to| {
        assert_eq!(from, OLD);
        assert_eq!(to, NEW);
        Ok("ahead".into())
    })
    .unwrap();
    assert!(matches!(result, Check::Available(_)));
    for status in ["identical", "behind", "diverged"] {
        assert!(matches!(
            assess(
                release.clone(),
                "2.0.0",
                Some(OLD),
                |_, _| Ok(status.into())
            )
            .unwrap(),
            Check::Current(_)
        ));
    }
    for revision in [Some(NEW), None, Some("invalid")] {
        assert!(matches!(
            assess(release.clone(), "2.0.0", revision, |_, _| panic!(
                "unexpected comparison"
            ))
            .unwrap(),
            Check::Current(_)
        ));
    }
    assert!(matches!(
        assess(release.clone(), "2.1.0", Some(OLD), |_, _| panic!(
            "unexpected comparison"
        ))
        .unwrap(),
        Check::Current(_)
    ));
    assert!(matches!(
        assess(
            parse(&fixture("2.10.0")).unwrap(),
            "2.9.0",
            Some(OLD),
            |_, _| panic!("unexpected comparison")
        )
        .unwrap(),
        Check::Available(_)
    ));
    assert!(assess(release, "2.0.0", Some(OLD), |_, _| Err("offline".into())).is_err());
}

#[test]
fn verifies_download_before_installing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("fixture");
    let release = parse(&fixture("2.0.0")).unwrap();
    fs::write(&path, b"abc").unwrap();
    verify_package(&path, &release).unwrap();
    fs::write(&path, b"abd").unwrap();
    assert!(
        verify_package(&path, &release)
            .unwrap_err()
            .contains("SHA-256")
    );
    fs::write(&path, b"abcd").unwrap();
    assert!(
        verify_package(&path, &release)
            .unwrap_err()
            .contains("size")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn installation_requires_a_separate_confirm_click() {
    let context = egui::Context::default();
    let mut updater = Updater {
        state: State::Available(Target::Debian, parse(&fixture("2.0.0")).unwrap()),
        pending: None,
    };
    let render = |updater: &mut Updater, events| {
        context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 500.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| updater.show(ui, Theme::dark()))
                });
            },
        )
    };
    let click = |updater: &mut Updater, label: &str| {
        let output = render(updater, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {label}"));
        for pressed in [true, false] {
            render(
                updater,
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
    };
    click(&mut updater, "Install update…");
    assert!(matches!(updater.state, State::Confirm(..)));
    assert!(updater.pending.is_none());
    click(&mut updater, "Cancel");
    assert!(matches!(updater.state, State::Available(..)));
    assert!(updater.pending.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn portable_install_rolls_back_if_pair_verification_fails() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("prefix");
    let bin = prefix.join("bin");
    let source = temp.path().join("source");
    let work = temp.path().join("work");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&work).unwrap();
    for name in ["rterm", "rterm-agent"] {
        fs::write(bin.join(name), b"old executable").unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(source.join(name), b"new executable").unwrap();
    }
    // Test the actual archive extraction and installer execution in a private
    // synthetic prefix, never the running RTerm install or the user's home.
    let archive = temp.path().join("fixture.tar.gz");
    let pack = || {
        let mut cmd = Command::new("/usr/bin/tar");
        cmd.args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg(".");
        run(cmd, "fixture archive").unwrap();
    };
    fs::write(source.join("install.sh"), b"set -e\ninstall -m755 rterm \"$PREFIX/bin/rterm\"\ninstall -m755 rterm-agent \"$PREFIX/bin/rterm-agent\"\n").unwrap();
    pack();
    install_portable(&archive, &work, &prefix).unwrap();
    assert_eq!(fs::read(bin.join("rterm")).unwrap(), b"new executable");
    assert_eq!(
        fs::read(bin.join("rterm-agent")).unwrap(),
        b"new executable"
    );

    let work = temp.path().join("work-failure");
    fs::create_dir(&work).unwrap();
    fs::write(source.join("rterm"), b"broken replacement").unwrap();
    fs::write(source.join("rterm-agent"), b"broken replacement").unwrap();
    fs::write(
        source.join("install.sh"),
        b"set -e\ninstall -m755 rterm \"$PREFIX/bin/rterm\"\nexit 1\n",
    )
    .unwrap();
    pack();
    assert!(
        install_portable(&archive, &work, &prefix)
            .unwrap_err()
            .contains("restored")
    );
    for name in ["rterm", "rterm-agent"] {
        assert_eq!(fs::read(bin.join(name)).unwrap(), b"new executable");
        assert_eq!(
            fs::metadata(bin.join(name)).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn portable_updates_lock_out_other_installers() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("bin")).unwrap();
    let lock = lock_portable(temp.path()).unwrap();
    assert!(lock_portable(temp.path()).is_err());
    drop(lock);
    assert!(lock_portable(temp.path()).is_ok());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "read-only live GitHub release metadata check"]
fn live_official_update_metadata() {
    let result = check(&Target::Portable(PathBuf::from("/fixture"))).unwrap();
    println!("Live release check: {result:?}");
}

fn response(code: i32, error: &str, body: &[u8]) -> Attempt {
    Attempt {
        code: Some(code),
        error: error.into(),
        body: body.into(),
    }
}

#[test]
fn transient_dns_failure_recovers_on_normal_retry() {
    let mut calls = 0;
    let body = retry_request(
        |mappings| {
            assert!(mappings.is_empty());
            calls += 1;
            Ok(if calls == 1 {
                response(6, "curl: (6) Could not resolve host: api.github.com", b"")
            } else {
                response(0, "", b"release metadata")
            })
        },
        |_| panic!("ordinary retry should recover without a resolver override"),
        |_| {},
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(body, b"release metadata");
}

#[test]
fn persistent_curl_dns_failure_uses_native_ipv4_and_ipv6_addresses() {
    let mut calls = 0;
    let mut lookups = Vec::new();
    let body = retry_request(
        |mappings| {
            calls += 1;
            if calls <= 2 {
                assert!(mappings.is_empty());
                return Ok(response(
                    6,
                    "curl: (6) Could not resolve host: api.github.com",
                    b"",
                ));
            }
            assert_eq!(mappings, ["api.github.com:443:192.0.2.1,[2001:db8::1]"]);
            Ok(response(0, "", b"verified response"))
        },
        |host| {
            lookups.push(host.to_owned());
            Ok(vec![
                "192.0.2.1".parse().unwrap(),
                "2001:db8::1".parse().unwrap(),
                "192.0.2.1".parse().unwrap(),
            ])
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(lookups, ["api.github.com"]);
    assert_eq!(body, b"verified response");
}

#[test]
fn download_dns_retry_handles_official_redirect_hosts() {
    let mut calls = 0;
    retry_request(
        |mappings| {
            calls += 1;
            Ok(match calls {
                1 | 2 => {
                    assert!(mappings.is_empty());
                    response(6, "curl: (6) Could not resolve host: github.com", b"")
                }
                3 => {
                    assert_eq!(mappings, ["github.com:443:192.0.2.1"]);
                    response(
                        6,
                        "curl: (6) Could not resolve host: release-assets.githubusercontent.com",
                        b"",
                    )
                }
                4 => {
                    assert_eq!(
                        mappings,
                        [
                            "github.com:443:192.0.2.1",
                            "release-assets.githubusercontent.com:443:192.0.2.1"
                        ]
                    );
                    response(0, "", b"")
                }
                _ => panic!("too many attempts"),
            })
        },
        |_| Ok(vec!["192.0.2.1".parse().unwrap()]),
        |_| {},
    )
    .unwrap();
    assert_eq!(calls, MAX_ATTEMPTS);
}

#[test]
fn unrecoverable_dns_stops_and_preserves_actionable_error() {
    let mut calls = 0;
    let mut lookups = 0;
    let error = retry_request(
        |mappings| {
            assert!(mappings.is_empty());
            calls += 1;
            Ok(response(
                6,
                "curl: (6) Could not resolve host: api.github.com",
                b"",
            ))
        },
        |_| {
            lookups += 1;
            Err("system resolver unavailable".into())
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(calls, MAX_ATTEMPTS);
    assert_eq!(lookups, 1);
    assert!(error.contains("DNS lookup for api.github.com failed after retrying"));
}

#[test]
fn retry_never_overrides_untrusted_hosts_or_certificate_errors() {
    assert_eq!(
        failed_dns_host("curl: (6) Could not resolve host: api.github.com.evil.example"),
        None
    );
    assert_eq!(
        failed_dns_host("curl: (6) Could not resolve host: github.com:443"),
        None
    );
    assert_eq!(
        failed_dns_host("curl: (5) Could not resolve proxy: api.github.com"),
        None
    );
    let mut calls = 0;
    let error = retry_request(
        |_| {
            calls += 1;
            Ok(response(60, "SSL certificate problem", b""))
        },
        |_| panic!("certificate errors must not invoke resolver fallback"),
        |_| panic!("certificate errors must not be retried"),
    )
    .unwrap_err();
    assert_eq!(calls, 1);
    assert!(error.contains("certificate verification failed"));
    assert!(!retryable(Some(22)));
    assert!(!retryable(Some(23)));
    assert!(!retryable(Some(63)));
}

#[test]
fn macos_update_packages_match_both_architectures() {
    let mut published = fixture("2.0.0");
    for suffix in ["-macos-arm64.pkg", "-macos-x64.pkg"] {
        let name = format!("RTerm-2.0.0{suffix}");
        published["assets"].as_array_mut().unwrap().push(json!({
            "name": name,
            "state": "uploaded",
            "size": 3,
            "digest": format!("sha256:{}", sha256(b"abc")),
            "browser_download_url": format!("{REPOSITORY}/releases/download/rterm-{NEW}/{name}"),
        }));
    }
    let bytes = serde_json::to_vec(&published).unwrap();
    for (silicon, expected) in [
        (true, "RTerm-2.0.0-macos-arm64.pkg"),
        (false, "RTerm-2.0.0-macos-x64.pkg"),
    ] {
        assert_eq!(
            select_release(&bytes, macos_package_suffix(silicon))
                .unwrap()
                .name,
            expected
        );
    }
}

#[test]
#[ignore = "read-only live native DNS and HTTPS verification"]
fn live_native_dns_fallback() {
    let addresses = system_resolve("api.github.com").unwrap();
    let mapping = resolve_argument("api.github.com", &addresses).unwrap();
    let output = curl()
        .unwrap()
        .args([
            "--max-time",
            "20",
            "--resolve",
            &mapping,
            "--max-filesize",
            &MAX_METADATA.to_string(),
        ])
        .arg(format!("{API}/releases/latest"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The same actual release JSON must be valid on Linux and both Mac architectures.
    for suffix in ["-linux-amd64.tar.gz", "-macos-arm64.pkg", "-macos-x64.pkg"] {
        let release = select_release(&output.stdout, suffix).unwrap();
        println!("Verified live release metadata: {}", release.name);
    }
}

#[test]
fn update_page_shows_only_essential_controls_and_status() {
    let context = egui::Context::default();
    let mut updater = Updater::default();
    let render = |updater: &mut Updater| {
        context
            .run_test_pass(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 450.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    ctx.root_ui(|root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            updater.show(ui, Theme::dark());
                        })
                    });
                },
            )
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let idle = render(&mut updater);
    assert_eq!(idle.len(), 4, "{idle:?}");
    assert!(idle.iter().any(|label| label == "Installed version"));
    assert!(idle.iter().any(|label| label == "Check for updates"));
    assert!(idle.iter().any(|label| label == "Release notes"));

    updater.state = State::Current(
        "This build is on a different development branch. No automatic update is offered.\nLatest public release: 2.0.0 · fixture".into(),
    );
    let current = render(&mut updater);
    assert_eq!(current.len(), 5, "{current:?}");
    assert!(current.iter().any(|label| label == "No updates available"));
    assert!(
        !current
            .iter()
            .any(|label| label.contains("development branch"))
    );

    updater.state = State::Checking;
    let checking = render(&mut updater);
    assert!(
        checking
            .iter()
            .any(|label| label == "Checking for updates…")
    );
    assert!(
        !checking
            .iter()
            .any(|label| label == "Check for updates" || label == "Release notes")
    );

    updater.state = State::Installing { keep_open: true };
    let installing = render(&mut updater);
    assert!(
        installing
            .iter()
            .any(|label| label == "Installing… Keep RTerm open.")
    );
    assert!(
        !installing
            .iter()
            .any(|label| label == "Check for updates" || label == "Release notes")
    );
}

#[test]
fn update_version_and_actions_have_clear_horizontal_spacing() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let t = Theme::dark();
    t.apply(&context);
    let mut updater = Updater::default();
    for width in [330.0, 540.0] {
        let output = context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 400.0),
                )),
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| updater.show(ui, t))
                });
            },
        );
        let label = |predicate: &dyn Fn(&str) -> bool| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if predicate(text.galley.text()) => {
                        // Wrapped horizontal labels encode their indentation
                        // in the first row, not in TextShape::pos.
                        Some(
                            text.galley.rows[0]
                                .rect_without_leading_space()
                                .translate(text.pos.to_vec2()),
                        )
                    }
                    _ => None,
                })
                .expect("update page label")
        };
        let name = label(&|s| s == "Installed version");
        let value = label(&|s| s.starts_with(VERSION));
        assert!((name.center().y - value.center().y).abs() < 1.0);
        assert!(
            value.left() - name.right() >= 15.0,
            "version label and value are too close: {name:?}, {value:?}"
        );
        let check = label(&|s| s == "Check for updates");
        let notes = label(&|s| s == "Release notes");
        assert!(
            notes.left() - check.right() >= 12.0,
            "actions are too close"
        );
    }
}
