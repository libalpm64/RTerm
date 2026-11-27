use super::*;

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
