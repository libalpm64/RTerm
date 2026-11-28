use super::*;
use crate::egui_test::TestContextExt;

fn key(algorithm: &str, fingerprint: &str) -> Key {
    Key {
        algorithm: algorithm.into(),
        fingerprint: fingerprint.into(),
    }
}

fn fixture(count: usize) -> KnownHosts {
    let mut viewer = KnownHosts::new(&Store::new(false));
    let entries = (0..count)
        .map(|i| {
            let host =
                crate::host_keys::target(&format!("server-{i:05}.example"), 22 + (i % 100) as u16);
            let key = key(
                if i % 2 == 0 { "ssh-ed25519" } else { "ssh-rsa" },
                &format!("SHA256:fixture-{i:05}"),
            );
            (host, key)
        })
        .collect();
    viewer.load(Ok(entries));
    viewer
}

#[test]
fn search_and_sort_large_host_snapshots() {
    let mut viewer = fixture(10_000);
    assert_eq!(viewer.visible.len(), 10_000);
    viewer.query = "SERVER-00042.EXAMPLE:64 ssh-ED25519".into();
    viewer.change_query();
    assert_eq!(viewer.visible.len(), 1);
    let index = viewer.visible[0];
    assert_eq!(viewer.rows[index].port, Some(64));
    viewer.selected.insert(index);
    viewer.query = "sha256:FIXTURE-00043".into();
    viewer.change_query();
    assert_eq!(viewer.visible.len(), 1);
    assert!(
        viewer.selected.is_empty(),
        "filter changes clear hidden selections"
    );
    assert_eq!(viewer.rows[viewer.visible[0]].algorithm, "ssh-rsa");
    viewer.query.clear();
    viewer.change_query();
    viewer.change_sort(Sort::Port);
    assert_eq!(viewer.rows[viewer.visible[0]].port, Some(22));
    viewer.selected.insert(viewer.visible[0]);
    let before = viewer.expected_keys();
    viewer.change_sort(Sort::Port);
    assert_eq!(viewer.rows[viewer.visible[0]].port, Some(121));
    assert_eq!(
        viewer.expected_keys(),
        before,
        "sort cannot retarget a selection"
    );
    viewer.change_sort(Sort::Algorithm);
    assert_eq!(viewer.rows[viewer.visible[0]].algorithm, "ssh-ed25519");
    viewer.change_sort(Sort::Fingerprint);
    assert_eq!(
        viewer.rows[viewer.visible[0]].fingerprint,
        "SHA256:fixture-00000"
    );
    assert!(viewer.reset_scroll);
}

#[test]
fn ipv6_and_legacy_targets_remain_distinct() {
    let row = HostRow::new(
        crate::host_keys::target("[2001:db8::1]", 2222),
        key("ssh-ed25519", "SHA256:fixture"),
    );
    assert_eq!(row.host, "2001:db8::1");
    assert_eq!(row.port, Some(2222));
    assert!(row.search.contains("[2001:db8::1]:2222"));
    let row = HostRow::new("legacy target".into(), key("ssh-rsa", "SHA256:legacy"));
    assert_eq!(row.port, None);
    assert_eq!(row.target, "legacy target");
}

#[test]
fn refresh_and_forget_preserve_the_live_trust_store() {
    let mut store = Store::new(false);
    let first = crate::host_keys::target("first.example", 22);
    let second = crate::host_keys::target("second.example", 22);
    let old = key("ssh-ed25519", "SHA256:old");
    let rotated = key("ssh-ed25519", "SHA256:rotated");
    store.change(&first, None, Some(old.clone())).unwrap();
    store.change(&second, None, Some(old.clone())).unwrap();
    let mut viewer = KnownHosts::new(&store);
    viewer.selected = viewer.visible.iter().copied().collect();
    viewer.confirming = true;
    store
        .change(&second, Some(&old), Some(rotated.clone()))
        .unwrap();
    viewer.forget(&mut store);
    assert!(viewer.error.is_some());
    assert!(viewer.selected.is_empty());
    assert!(!viewer.confirming);
    assert_eq!(store.list().unwrap()[&first], old);
    assert_eq!(store.list().unwrap()[&second], rotated);
    viewer.query = "first.example".into();
    viewer.change_query();
    viewer.selected = viewer.visible.iter().copied().collect();
    viewer.confirming = true;
    viewer.forget(&mut store);
    assert!(viewer.error.is_none());
    assert!(viewer.visible.is_empty());
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.list().unwrap()[&second], rotated);
}

#[test]
fn renders_only_visible_rows_and_can_scroll_to_the_last_host() {
    let mut viewer = fixture(10_000);
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    Theme::dark().apply(&context);
    let render = |viewer: &mut KnownHosts, events: Vec<egui::Event>| {
        let mut result = None;
        let frame = context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 480.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        result = Some(viewer.table(ui));
                    })
                });
            },
        );
        (result.unwrap(), frame.shapes.len())
    };
    let (top, shapes) = render(&mut viewer, vec![]);
    assert_eq!(top.inner.start, 0);
    assert!(
        top.inner.len() < 32,
        "rendered {:?} instead of only the viewport",
        top.inner
    );
    assert!(shapes < 400, "too many paint shapes: {shapes}");
    let pointer = top.inner_rect.center();
    let _ = render(
        &mut viewer,
        vec![
            egui::Event::PointerMoved(pointer),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, -2.0 * top.content_size.y),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    for _ in 0..30 {
        let _ = render(&mut viewer, vec![]);
    }
    let (bottom, shapes) = render(&mut viewer, vec![]);
    assert_eq!(bottom.inner.end, 10_000);
    assert!(bottom.inner.len() < 32, "{:?}", bottom.inner);
    assert!(shapes < 400, "too many paint shapes: {shapes}");
    viewer.query = "server-00001".into();
    viewer.change_query();
    let (filtered, _) = render(&mut viewer, vec![]);
    assert_eq!(filtered.inner, 0..1);
    eprintln!(
        "10,000 keys: top {:?}, bottom {:?}, {} paint shapes at bottom",
        top.inner, bottom.inner, shapes
    );
}

#[test]
fn modal_fits_typical_and_compact_desktops() {
    let mut viewer = fixture(10_000);
    let mut store = Store::new(false);
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    Theme::dark().apply(&context);
    for size in [vec2(1200.0, 800.0), vec2(640.0, 420.0), vec2(500.0, 340.0)] {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), size);
        for _ in 0..3 {
            let _ = context.run_test_pass(
                egui::RawInput {
                    screen_rect: Some(rect),
                    ..Default::default()
                },
                |ctx| {
                    assert!(!viewer.show(ctx, &mut store, Theme::dark()));
                },
            );
        }
        let area = context
            .memory(|memory| memory.area_rect(egui::Id::unique("known-ssh-hosts")))
            .unwrap();
        assert!(
            rect.expand(1.0).contains_rect(area),
            "screen {rect:?}, modal {area:?}"
        );
    }
    viewer.selected = viewer.visible.iter().copied().collect();
    viewer.confirming = true;
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 340.0));
    for _ in 0..3 {
        let _ = context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ctx| assert!(!viewer.show(ctx, &mut store, Theme::dark())),
        );
    }
    let area = context
        .memory(|memory| memory.area_rect(egui::Id::unique("known-ssh-hosts")))
        .unwrap();
    assert!(
        rect.expand(1.0).contains_rect(area),
        "confirmation {area:?}"
    );
    assert!(viewer.confirming);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Wayland desktop; captures an isolated synthetic Known Hosts preview"]
fn native_known_hosts_preview() {
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    let path = std::path::PathBuf::from("target/linux-known-hosts-fixture-preview.png");
    struct Preview {
        screenshot: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<egui::ColorImage>>>>,
        viewer: KnownHosts,
        store: Store,
        frames: u32,
        path: std::path::PathBuf,
    }
    impl eframe::App for Preview {
        fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = &root.ctx().clone();
            egui::CentralPanel::default().show(root, |ui| {
                ui.heading("RTerm — synthetic Known Hosts fixture");
            });
            self.viewer.show(root, &mut self.store, Theme::dark());
            if let Some(image) = self.screenshot.lock().unwrap().take() {
                image::save_buffer(
                    &self.path,
                    image.as_raw(),
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.frames += 1;
            if self.frames == 5 {
                let screenshot = self.screenshot.clone();
                let repaint = ctx.clone();
                ctx.request_screenshot(move |image| {
                    *screenshot.lock().unwrap() = Some(image);
                    repaint.request_repaint();
                });
            }
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }
    let output = path.clone();
    eframe::run_native(
        "rterm-known-hosts-fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("RTerm Known Hosts fixture")
                .with_inner_size([1100.0, 750.0]),
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_wayland().with_any_thread(true);
            })),
            ..Default::default()
        },
        Box::new(move |cc| {
            crate::theme::install_fonts(&cc.egui_ctx);
            Theme::dark().apply(&cc.egui_ctx);
            let mut viewer = fixture(10_000);
            for (index, row) in viewer.rows.iter_mut().enumerate() {
                row.fingerprint =
                    format!("SHA256:fixture{index:05}abcdefghijklmnopqrstuvwxyz01234");
            }
            viewer.selected.insert(2);
            viewer.selected.insert(6);
            Ok(Box::new(Preview {
                screenshot: Default::default(),
                viewer,
                store: Store::new(false),
                frames: 0,
                path: output,
            }))
        }),
    )
    .unwrap();
    assert!(path.is_file());
    eprintln!("Synthetic fixture screenshot: {}", path.display());
}
