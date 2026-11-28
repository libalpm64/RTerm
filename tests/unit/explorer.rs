use super::*;

#[test]
fn paired_file_panes_align_paths_headers_and_rows() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    app.explorer.local.replace(
        PathBuf::from("/fixture/local"),
        vec![fixture_entry("local.txt", false, None)],
    );
    app.explorer.accept_initial_listing(
        42,
        "/fixture/remote".into(),
        vec![fixture_entry("remote.txt", false, None)],
    );
    app.explorer.expanded = true;
    let render = |app: &mut App, width| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 600.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.explorer_workspace(ui, ui.max_rect()));
            },
        )
    };
    let labels = |output: &egui::FullOutput, name: &str| {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == name => Some(text.pos),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    for (width, fraction) in [(1200.0, 0.5), (800.0, 0.75), (800.0, 0.25)] {
        app.explorer.split_fraction = fraction;
        for loading in [false, true] {
            app.explorer.loading = loading;
            let output = render(&mut app, width);
            for heading in ["Name", "Date Modified", "..."] {
                let positions = labels(&output, heading);
                assert_eq!(positions.len(), 2, "{heading}");
                assert_eq!(positions[0].y, positions[1].y, "{heading}");
            }
            let local = labels(&output, "/fixture/local");
            let remote = labels(&output, "/fixture/remote");
            assert_eq!(local.len(), 1);
            assert_eq!(remote.len(), 1);
            assert_eq!(local[0].y, remote[0].y);
            assert_eq!(
                labels(&output, "local.txt")[0].y,
                labels(&output, "remote.txt")[0].y
            );
            if width == 1200.0 {
                let status = if loading {
                    "SFTP · Loading folder..."
                } else {
                    "SFTP · Connected"
                };
                assert_eq!(
                    labels(&output, status)[0].y,
                    labels(&output, "Local files")[0].y
                );
                let sizes = labels(&output, "Size");
                assert_eq!(sizes.len(), 2);
                assert_eq!(sizes[0].y, sizes[1].y);
            }
        }
    }
    app.explorer.loading = false;
    let compact = render(&mut app, 350.0);
    assert_eq!(labels(&compact, "Connected").len(), 1);
    assert_eq!(labels(&compact, "Name").len(), 1);
}

#[test]
fn path_field_keeps_geometry_on_hover_and_focus() {
    let context = egui::Context::default();
    let theme = theme::Theme::dark();
    theme.apply(&context);
    theme::install_fonts(&context);
    let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(180.0, 24.0));
    let id = egui::Id::new("test-path");
    let mut path = "/home/user".to_owned();
    let render = |path: &mut String, events: Vec<egui::Event>| {
        let mut geometry = None;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let before = ui.min_rect();
                    let output = path_field(ui, rect, id, path, theme);
                    assert_eq!(
                        ui.min_rect(),
                        before,
                        "path field must not expand its parent"
                    );
                    geometry = Some((
                        output.response.rect,
                        output.galley_pos,
                        output.text_clip_rect,
                    ));
                });
            },
        );
        geometry.unwrap()
    };
    let baseline = render(&mut path, vec![]);
    assert_eq!(baseline.0, rect);
    assert_eq!(baseline.2, rect);
    let hover = rect.center();
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(hover)]),
        baseline
    );
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(rect.right_top())]),
        baseline
    );
    context.memory_mut(|memory| memory.request_focus(id));
    assert_eq!(render(&mut path, vec![]), baseline);
    context.memory_mut(|memory| memory.surrender_focus(id));
    path = "/very/long/remote/directory/that/must/not/expand/the/location/bar".into();
    let long = render(&mut path, vec![]);
    assert_eq!(long.0, rect);
    assert_eq!(long.2, rect);
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(hover)]),
        long
    );
}

#[test]
fn hidden_entries_can_be_filtered_without_reloading() {
    let entry = |name: &str, directory, size| Entry {
        name: name.into(),
        directory,
        size,
        permissions: None,
        modified: None,
    };
    let mut listing = Listing::new(".".to_owned());
    listing.replace(
        "/home/user".into(),
        vec![
            entry(".", true, 0),
            entry("..", true, 0),
            entry(".config", true, 0),
            entry(".bashrc", false, 20),
            entry("Documents", true, 0),
            entry("report.txt", false, 10),
        ],
    );
    assert_eq!(listing.entries.len(), 4);
    assert_eq!(listing.total_bytes, 30);
    assert_eq!(listing.non_hidden_bytes, 10);
    assert_eq!(
        listing
            .non_hidden
            .iter()
            .map(|&i| listing.entries[i].name.as_str())
            .collect::<Vec<_>>(),
        vec!["Documents", "report.txt"]
    );
    assert_eq!(navigation_parent(&listing.path), Some("/home"));
    listing.replace("/".into(), vec![entry(".hidden", false, 5)]);
    assert!(listing.non_hidden.is_empty());
    assert_eq!(listing.non_hidden_bytes, 0);
    assert_eq!(listing.total_bytes, 5);
}

#[test]
fn parent_navigation_is_separate_from_real_files() {
    let entry = |name: &str, directory| Entry {
        name: name.into(),
        directory,
        size: 10,
        permissions: None,
        modified: None,
    };
    let mut listing = Listing::new(".".to_owned());
    listing.replace(
        "/home/user/".into(),
        vec![
            entry(".", true),
            entry("..", true),
            entry("file", false),
            entry("Zfolder", true),
        ],
    );
    assert_eq!(navigation_parent(&listing.path), Some("/home"));
    assert_eq!(navigation_parent("/home"), Some("/"));
    assert_eq!(navigation_parent("/"), None);
    assert_eq!(navigation_parent("."), None);
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Zfolder", "file"]
    );
    assert_eq!(listing.total_bytes, 10);
    assert!(listing.selected.is_empty());
}

fn fixture_entry(name: &str, directory: bool, day: Option<u32>) -> Entry {
    Entry {
        name: name.into(),
        directory,
        size: if directory { 0 } else { 10 },
        permissions: None,
        modified: day.map(|day| {
            use chrono::TimeZone;
            chrono::Local
                .with_ymd_and_hms(2026, 11, day, 12, 0, 0)
                .single()
                .unwrap()
        }),
    }
}

#[test]
fn modified_range_is_inclusive_preserves_folders_and_clears_hidden_selection() {
    let mut listing = Listing::new("/fixture".to_owned());
    listing.replace(
        "/fixture".into(),
        vec![
            fixture_entry("old", false, Some(1)),
            fixture_entry("recent", false, Some(25)),
            fixture_entry("middle", false, Some(15)),
            fixture_entry("unknown", false, None),
            fixture_entry("folder", true, Some(1)),
            fixture_entry(".hidden", false, Some(25)),
        ],
    );
    let lo = modified_day(&fixture_entry("lo", false, Some(15))).unwrap();
    let hi = modified_day(&fixture_entry("hi", false, Some(25))).unwrap();
    listing.selected.extend(
        ["old", "recent", "middle", "unknown"]
            .into_iter()
            .map(str::to_owned),
    );
    listing.date_filter = Some((lo, hi));
    listing.date_sort = Some(true);
    listing.sort_entries();
    listing.reindex();
    let names = |indices: &[usize]| {
        indices
            .iter()
            .map(|&i| listing.entries[i].name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&listing.visible),
        vec!["folder", ".hidden", "recent", "middle"]
    );
    assert_eq!(
        names(&listing.non_hidden),
        vec!["folder", "recent", "middle"]
    );
    assert_eq!(listing.total_bytes, 30);
    assert_eq!(listing.non_hidden_bytes, 20);
    assert_eq!(
        listing.selected,
        HashSet::from(["recent".into(), "middle".into()])
    );
    let entries = listing.entries.clone();
    listing.replace("/fixture".into(), entries.clone());
    assert_eq!(listing.date_filter, Some((lo, hi)));
    listing.replace("/different".into(), entries);
    assert!(listing.date_filter.is_none());
}

#[test]
fn file_drag_targets_reject_self_descendants_and_another_server() {
    let local = DragFiles(vec![Target::Local {
        path: PathBuf::from("/fixture/folder"),
        entry: fixture_entry("folder", true, None),
    }]);
    assert!(!can_drop(
        &local,
        &Destination::Local("/fixture/folder".into())
    ));
    assert!(!can_drop(
        &local,
        &Destination::Local("/fixture/folder/child".into())
    ));
    assert!(can_drop(
        &local,
        &Destination::Remote(1, "/incoming".into())
    ));
    let remote = DragFiles(vec![Target::Remote {
        id: 1,
        path: "/fixture/folder".into(),
        entry: fixture_entry("folder", true, None),
    }]);
    assert!(!can_drop(
        &remote,
        &Destination::Remote(2, "/incoming".into())
    ));
    assert!(!can_drop(
        &remote,
        &Destination::Remote(1, "/fixture/folder/child".into())
    ));
    assert!(can_drop(&remote, &Destination::Local("/downloads".into())));
    assert!(can_drop(
        &remote,
        &Destination::Remote(1, "/fixture/other".into())
    ));
}

#[test]
fn local_file_drag_hovers_open_folder_and_prompts_for_move() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("destination")).unwrap();
    std::fs::write(temp.path().join("source.txt"), b"fixture").unwrap();
    let ctx = egui::Context::default();
    let mut app = App::test_fixture(&ctx);
    app.explorer.load_local(temp.path().to_owned());
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
    let render = |app: &mut App, events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        app.explorer_side(ui, rect, false, false);
                    });
            },
        )
    };
    let output = render(&mut app, vec![]);
    let position = |name: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == name
                {
                    Some(text.pos + text.galley.size() * 0.5)
                } else {
                    None
                }
            })
            .unwrap()
    };
    let from = position("source.txt");
    let to = position("destination");
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    render(&mut app, vec![egui::Event::PointerMoved(from)]);
    render(&mut app, vec![button(from, true)]);
    render(&mut app, vec![egui::Event::PointerMoved(to)]);
    assert!(egui::DragAndDrop::has_payload_of_type::<DragFiles>(&ctx));
    render(&mut app, vec![]);
    assert!(app.explorer.hover_folder.is_some());
    app.explorer.hover_folder.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(700);
    render(&mut app, vec![]);
    assert_eq!(
        app.explorer.local.path,
        temp.path().join("destination").canonicalize().unwrap()
    );
    let empty = pos2(300.0, 250.0);
    render(&mut app, vec![egui::Event::PointerMoved(empty)]);
    render(&mut app, vec![button(empty, false)]);
    assert!(
        matches!(&app.dialog,Some(Dialog::Files{action:FileAction::Move,entries,input}) if entries.len()==1 && input.ends_with("/destination"))
    );
    assert!(
        temp.path().join("source.txt").is_file(),
        "move requires confirmation"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "native Wayland file manager screenshot using synthetic files only"]
fn native_sftp_workspace_preview() {
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    struct Preview {
        app: App,
        frames: u32,
    }
    impl eframe::App for Preview {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            if let Some(image) = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            }) {
                image::save_buffer(
                    "target/linux-sftp-workspace-preview.png",
                    image.as_raw(),
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    self.app.explorer_workspace(ui, ui.max_rect());
                });
            self.frames += 1;
            if self.frames == 5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }
    eframe::run_native(
        "rterm-sftp-fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("RTerm file manager fixture")
                .with_inner_size([1280.0, 780.0]),
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_wayland().with_any_thread(true);
            })),
            ..Default::default()
        },
        Box::new(|cc| {
            let mut app = App::test_fixture(&cc.egui_ctx);
            app.explorer.local.replace(
                PathBuf::from("/home/fixture/Documents"),
                vec![
                    fixture_entry("Archives", true, Some(4)),
                    fixture_entry("Projects", true, Some(17)),
                    fixture_entry("report.pdf", false, Some(15)),
                    fixture_entry("server-backup.tar.gz", false, Some(25)),
                ],
            );
            app.explorer.accept_initial_listing(
                42,
                "/srv/www".into(),
                vec![
                    fixture_entry("assets", true, Some(20)),
                    fixture_entry("logs", true, Some(10)),
                    fixture_entry("index.html", false, Some(15)),
                    fixture_entry("config.toml", false, Some(5)),
                ],
            );
            app.explorer.expanded = true;
            app.explorer.remote.filter_open = true;
            app.explorer.remote.date_filter = app.explorer.remote.date_bounds;
            Ok(Box::new(Preview { app, frames: 0 }))
        }),
    )
    .unwrap();
    assert!(Path::new("target/linux-sftp-workspace-preview.png").is_file());
}

#[test]
fn local_folder_creation_uses_current_directory_and_never_overwrites() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let temp = tempfile::tempdir().unwrap();
    app.explorer.load_local(temp.path().to_owned());
    let directory = app.explorer.local.path.clone();
    assert!(app.can_create_local_folder());
    app.new_local_folder();
    let draft = app
        .explorer
        .folder_draft
        .take()
        .expect("local folder draft");
    assert_eq!(draft.destination, Destination::Local(directory.clone()));
    assert!(draft.name.is_empty());
    assert!(app.dialog.is_none());
    for invalid in [
        "",
        " ",
        ".",
        "..",
        "../escape",
        "nested/name",
        "nested\\name",
        "/absolute",
    ] {
        assert!(
            app.create_local_folder(&directory, invalid).is_err(),
            "{invalid}"
        );
    }
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);

    app.create_local_folder(&directory, "New local folder")
        .unwrap();
    assert!(directory.join("New local folder").is_dir());
    assert!(
        app.explorer
            .local
            .entries
            .iter()
            .any(|entry| entry.directory && entry.name == "New local folder")
    );
    assert!(
        app.create_local_folder(&directory, "New local folder")
            .is_err()
    );
    let file = directory.join("existing.txt");
    std::fs::write(&file, b"keep").unwrap();
    assert!(app.create_local_folder(&directory, "existing.txt").is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");

    app.explorer.load_local(directory.join("New local folder"));
    assert!(app.create_local_folder(&directory, "stale").is_err());
    assert!(!directory.join("stale").exists());
    app.explorer.load_local(directory.join("missing"));
    assert!(!app.can_create_local_folder());
}

#[cfg(unix)]
#[test]
fn local_folder_creation_rejects_retargeted_directory_and_existing_link() {
    use std::os::unix::fs::symlink;
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("local");
    let other = temp.path().join("other");
    std::fs::create_dir(&directory).unwrap();
    std::fs::create_dir(&other).unwrap();
    app.explorer.load_local(directory.clone());
    let directory = app.explorer.local.path.clone();
    symlink(&other, directory.join("existing")).unwrap();
    assert!(app.create_local_folder(&directory, "existing").is_err());
    assert!(
        directory
            .join("existing")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    std::fs::rename(&directory, temp.path().join("original")).unwrap();
    symlink(&other, &directory).unwrap();
    assert!(app.create_local_folder(&directory, "unexpected").is_err());
    assert!(!other.join("unexpected").exists());
}

#[test]
fn local_folder_plus_works_in_expanded_compact_and_docked_views() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let temp = tempfile::tempdir().unwrap();
    app.explorer.load_local(temp.path().to_owned());
    let directory = app.explorer.local.path.clone();
    for (expanded, width, docked, pos) in [
        (true, 1200.0, false, pos2(564.0, 43.0)),
        (true, 350.0, false, pos2(316.0, 65.0)),
        (false, 240.0, true, pos2(166.0, 35.0)),
    ] {
        app.explorer.expanded = expanded;
        app.explorer.remote_active = false;
        let render = |app: &mut App, events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            if docked {
                                app.explorer_panel(ui, ui.max_rect());
                            } else {
                                app.explorer_workspace(ui, ui.max_rect());
                            }
                        });
                },
            )
        };
        render(&mut app, vec![]);
        for pressed in [true, false] {
            render(
                &mut app,
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
        let draft = app.explorer.folder_draft.as_ref().unwrap_or_else(|| {
            panic!("local folder button not working at width {width}, docked {docked}")
        });
        assert_eq!(draft.destination, Destination::Local(directory.clone()));
        assert!(draft.name.is_empty());
        assert!(context.memory(|memory| memory.has_focus(draft.editor_id())));
        assert!(app.dialog.is_none());
        let name = format!("Folder at {width}");
        render(&mut app, vec![egui::Event::Text(name.clone())]);
        assert_eq!(app.explorer.folder_draft.as_ref().unwrap().name, name);
        assert!(
            !directory.join(&name).exists(),
            "draft must not create a folder before Enter"
        );
        render(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(
            app.explorer.folder_draft.is_none(),
            "Enter should commit the draft"
        );
        assert!(directory.join(&name).is_dir());
        // The same editor can be cancelled without creating a directory.
        app.new_local_folder();
        render(&mut app, vec![]);
        render(&mut app, vec![egui::Event::Text("cancelled folder".into())]);
        render(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(app.explorer.folder_draft.is_none(), "Escape must cancel");
        assert!(!directory.join("cancelled folder").exists());
        assert!(
            app.explorer
                .local
                .entries
                .iter()
                .any(|entry| entry.directory && entry.name == name)
        );
    }
}

#[test]
fn inline_folder_validation_keeps_editor_and_navigation_discards_draft() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let temp = tempfile::tempdir().unwrap();
    app.explorer.load_local(temp.path().to_owned());
    let dir = app.explorer.local.path.clone();
    std::fs::create_dir(dir.join("existing")).unwrap();
    app.explorer.refresh_local();
    app.new_local_folder();
    let render = |app: &mut App, events| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(240.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.explorer_panel(ui, ui.max_rect()));
            },
        )
    };
    let enter = || egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    render(&mut app, vec![]);
    render(
        &mut app,
        vec![egui::Event::Text("existing".into()), enter()],
    );
    let draft = app
        .explorer
        .folder_draft
        .as_ref()
        .expect("duplicate name should remain editable");
    assert_eq!(draft.name, "existing");
    assert!(draft.error.is_some());
    assert!(app.dialog.is_none() && app.message.is_none());
    render(&mut app, vec![]);
    assert!(context.memory(|memory| {
        memory.has_focus(app.explorer.folder_draft.as_ref().unwrap().editor_id())
    }));
    // Re-entering the same name does not overwrite anything.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    app.explorer.load_local(dir.join("existing"));
    assert!(app.explorer.folder_draft.is_none());
    assert_eq!(std::fs::read_dir(dir.join("existing")).unwrap().count(), 0);
}

#[test]
fn folder_and_remote_path_editor_state_stays_bounded_across_churn() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    app.explorer
        .local
        .replace(PathBuf::from("/fixture/local"), vec![]);
    app.explorer.expanded = true;
    let render = |app: &mut App, events| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.explorer_workspace(ui, ui.max_rect()));
            },
        )
    };
    for i in 1..=1000 {
        app.explorer
            .accept_initial_listing(i, format!("/fixture/remote-{i}"), vec![]);
        app.new_local_folder();
        let _ = render(&mut app, vec![]);
        let _ = render(&mut app, vec![egui::Event::Text(format!("Folder {i}"))]);
        assert_eq!(
            app.explorer.folder_draft.as_ref().unwrap().name,
            format!("Folder {i}")
        );
        app.explorer.folder_draft = None;
        app.explorer.prune_editor_state(&context);
        assert!(egui::text_edit::TextEditState::load(&context, folder_editor_id()).is_none());
        assert!(context.data(|data| data.count::<egui::text_edit::TextEditState>()) <= 2);
    }
    // Switching hosts must clear a focused path editor, including its undo history.
    context.memory_mut(|memory| memory.request_focus(path_editor_id(true)));
    app.explorer.remote.path_input = "/stale/path".into();
    app.explorer
        .accept_initial_listing(1001, "/fixture/new-host".into(), vec![]);
    let _ = render(&mut app, vec![]);
    assert_eq!(app.explorer.remote.path_input, "/fixture/new-host");
    assert!(!context.memory(|memory| memory.has_focus(path_editor_id(true))));
}
