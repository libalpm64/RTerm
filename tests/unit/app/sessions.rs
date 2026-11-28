use super::*;

fn fixture_profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        folder: None,
        connection: Connection::Local {
            shell: "/bin/fixture".into(),
        },
    }
}

fn render(app: &mut App, context: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 600.0))),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    app.session_panel(ui, ui.max_rect());
                });
        },
    )
}

fn click(app: &mut App, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [true, false] {
        let _ = render(
            app,
            ctx,
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
}

fn key(key: Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

fn label(output: &egui::FullOutput, name: &str) -> Option<egui::Pos2> {
    output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.text() == name => Some(text.pos),
        _ => None,
    })
}

#[test]
fn inline_session_folder_creation_cancel_and_rename() {
    let ctx = egui::Context::default();
    let mut app = App::test_fixture(&ctx);
    app.profiles.push(fixture_profile("Server A"));
    let _ = render(&mut app, &ctx, vec![]);
    click(&mut app, &ctx, pos2(286.0, 11.0));
    assert!(app.dialog.is_none());
    assert!(app.session_folder_ui.editing(&ctx));
    let _ = render(&mut app, &ctx, vec![egui::Event::Text("Production".into())]);
    assert!(app.session_folders.is_empty());
    let _ = render(&mut app, &ctx, vec![key(Key::Enter)]);
    assert!(app.session_folders.contains("Production"));
    assert!(app.session_folder_ui.draft.is_none());
    assert!(egui::text_edit::TextEditState::load(&ctx, egui::Id::new(FOLDER_EDITOR_ID)).is_none());

    click(&mut app, &ctx, pos2(286.0, 11.0));
    let _ = render(
        &mut app,
        &ctx,
        vec![egui::Event::Text("Production".into()), key(Key::Enter)],
    );
    assert!(
        app.session_folder_ui
            .draft
            .as_ref()
            .unwrap()
            .error
            .is_some()
    );
    let _ = render(&mut app, &ctx, vec![]);
    let _ = render(&mut app, &ctx, vec![key(Key::Escape)]);
    assert!(app.session_folder_ui.draft.is_none());
    assert_eq!(app.session_folders.len(), 1);

    let drag = SessionDrag::new(0, &app.profiles[0]).unwrap();
    app.move_saved_session(drag, Some("Production".into()))
        .unwrap();
    app.begin_session_folder(Some("Production".into()));
    app.session_folder_ui.draft.as_mut().unwrap().name = "Infrastructure".into();
    let _ = render(&mut app, &ctx, vec![]);
    let _ = render(&mut app, &ctx, vec![key(Key::Enter)]);
    assert!(!app.session_folders.contains("Production"));
    assert!(app.session_folders.contains("Infrastructure"));
    assert_eq!(app.profiles[0].folder.as_deref(), Some("Infrastructure"));
    assert_eq!(app.profiles[0].name, "Server A");
}

#[test]
fn session_folders_expand_filter_move_and_remove_without_deleting_profiles() {
    let ctx = egui::Context::default();
    let mut app = App::test_fixture(&ctx);
    app.session_folders
        .extend(["Production".into(), "Empty".into()]);
    app.profiles = vec![fixture_profile("Server A"), fixture_profile("Server B")];
    app.profiles[0].folder = Some("Production".into());
    let output = render(&mut app, &ctx, vec![]);
    let folder = label(&output, "Production").expect("folder label");
    assert!(label(&output, "Empty").is_some());
    assert!(label(&output, "Server A").unwrap().x > label(&output, "Server B").unwrap().x);
    click(&mut app, &ctx, folder + vec2(5.0, 6.0));
    let output = render(&mut app, &ctx, vec![]);
    assert!(app.session_folder_ui.collapsed.contains("Production"));
    assert!(label(&output, "Server A").is_none());
    app.filter = "Server A".into();
    let output = render(&mut app, &ctx, vec![]);
    assert!(label(&output, "Server A").is_some());
    assert!(label(&output, "Server B").is_none());
    assert!(label(&output, "Empty").is_none());
    assert!(app.session_folder_ui.collapsed.contains("Production"));
    app.filter.clear();
    let output = render(&mut app, &ctx, vec![]);
    let source = label(&output, "Server B").unwrap() + vec2(5.0, 6.0);
    let target = label(&output, "Production").unwrap() + vec2(5.0, 6.0);
    // Real pointer-driven drag/drop onto a collapsed folder.
    let _ = render(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(source),
            egui::Event::PointerButton {
                pos: source,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let _ = render(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(source + vec2(0.0, -8.0))],
    );
    assert!(egui::DragAndDrop::has_payload_of_type::<SessionDrag>(&ctx));
    let _ = render(&mut app, &ctx, vec![egui::Event::PointerMoved(target)]);
    let _ = render(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert_eq!(app.profiles[1].folder.as_deref(), Some("Production"));
    assert!(!app.session_folder_ui.collapsed.contains("Production"));
    let output = render(&mut app, &ctx, vec![]);
    let source = label(&output, "Server A").unwrap() + vec2(5.0, 6.0);
    let target = pos2(45.0, 11.0);
    let _ = render(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(source),
            egui::Event::PointerButton {
                pos: source,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let _ = render(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(source + vec2(0.0, -8.0))],
    );
    let _ = render(&mut app, &ctx, vec![egui::Event::PointerMoved(target)]);
    let _ = render(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(app.profiles[0].folder.is_none());
    assert_eq!(app.profiles[1].folder.as_deref(), Some("Production"));
    app.remove_session_folder("Production").unwrap();
    assert_eq!(app.profiles.len(), 2);
    assert!(app.profiles.iter().all(|p| p.folder.is_none()));
    assert!(app.session_folders.contains("Empty"));
}

#[test]
fn stale_drag_cannot_move_replaced_profile_and_lock_blocks_changes() {
    let ctx = egui::Context::default();
    let mut app = App::test_fixture(&ctx);
    app.session_folders.insert("Folder".into());
    app.profiles.push(fixture_profile("Original"));
    let drag = SessionDrag::new(0, &app.profiles[0]).unwrap();
    app.profiles[0] = fixture_profile("Replacement");
    assert!(app.move_saved_session(drag, Some("Folder".into())).is_err());
    assert!(app.profiles[0].folder.is_none());
    app.locked = true;
    assert!(app.remove_session_folder("Folder").is_err());
    assert!(app.session_folders.contains("Folder"));
}

#[test]
fn failed_metadata_save_rolls_back_folder_name_and_port() {
    let ctx = egui::Context::default();
    let mut app = App::test_fixture(&ctx);
    let mut profile = Profile {
        name: "Original".into(),
        folder: Some("Old".into()),
        connection: Connection::Telnet(crate::models::TelnetConfig {
            host: "fixture.invalid".into(),
            port: 23,
        }),
    };
    app.session_folders.insert("Old".into());
    app.profiles.push(profile.clone());
    let result = app.change_saved_metadata(
        |folders, profiles| {
            folders.clear();
            folders.insert("New".into());
            profiles[0].folder = Some("New".into());
            profiles[0].name = "Renamed".into();
            profiles[0].set_network_port(2323);
        },
        |app| {
            assert_eq!(app.profiles[0].name, "Renamed");
            assert_eq!(app.profiles[0].network_port(), Some(2323));
            Err("fixture write failure".into())
        },
    );
    assert_eq!(result.unwrap_err(), "fixture write failure");
    assert_eq!(app.session_folders, BTreeSet::from(["Old".into()]));
    assert_eq!(
        serde_json::to_value(&app.profiles[0]).unwrap(),
        serde_json::to_value(&profile).unwrap()
    );
    app.rename_saved_session(0, "Renamed".into()).unwrap();
    app.set_saved_session_port(0, 2323).unwrap();
    profile.name = "Renamed".into();
    profile.set_network_port(2323);
    assert_eq!(
        serde_json::to_value(&app.profiles[0]).unwrap(),
        serde_json::to_value(&profile).unwrap()
    );
}
