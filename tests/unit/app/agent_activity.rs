use super::*;
use crate::egui_test::TestContextExt;

impl Activity {
    pub(in crate::app) fn saved(&self) -> &VecDeque<AgentAuditEntry> {
        &self.saved
    }
}

#[test]
fn logged_activity_survives_live_history_churn_and_tracks_old_running_command() {
    let ctx = egui::Context::default();
    let bridge = agent::Bridge::start(&ctx).unwrap();
    let access = bridge.mcp_access();
    let mut activity = Activity::default();
    let submit = |activity: &mut Activity, i: usize, session_id: u32, logged: bool| {
        let operation = format!("fixture-{i}");
        let result = access.request(serde_json::json!({
            "action": "execute", "session_id": session_id,
            "operation_id": operation, "command": "fixture", "wait_ms": 0
        }));
        assert_eq!(result["ok"], true);
        let request = bridge.requests.try_recv().unwrap();
        assert!(
            request
                .reply
                .start(Instant::now() + Duration::from_secs(30))
        );
        let entry = AgentAuditEntry {
            id: operation.clone(),
            operation,
            session: "fixture".into(),
            session_name: "Fixture SSH".into(),
            action: "Background command".into(),
            command: format!("fixture-{i}"),
            submitted_ms: i as i64,
            finished_ms: None,
            phase: "queued".into(),
            exit_code: None,
            duration_ms: None,
            detail: None,
        };
        activity.track(entry, request.reply.clone(), request.active.clone());
        if logged {
            activity.retain_command(&request.reply);
        }
        activity.refresh();
        request.reply
    };
    let oldest = submit(&mut activity, 0, 1, true);
    for i in 1..=600 {
        let reply = submit(&mut activity, i, 2, true);
        reply
            .send(serde_json::json!({"ok":true,"exit_code":0}))
            .unwrap();
        activity.refresh();
    }
    assert_eq!(activity.saved.len(), 601);
    assert_eq!(activity.current.len(), LIVE_ACTIVITY_LIMIT + 1);
    assert!(
        activity
            .current
            .iter()
            .any(|item| item.entry.id == "fixture-0")
    );
    assert_eq!(activity.entries().len(), 601);
    oldest
        .send(serde_json::json!({"ok":true,"exit_code":7}))
        .unwrap();
    activity.refresh();
    assert_eq!(activity.current.len(), LIVE_ACTIVITY_LIMIT);
    let oldest = activity
        .saved
        .iter()
        .find(|entry| entry.id == "fixture-0")
        .unwrap();
    assert_eq!(oldest.exit_code, Some(7));
    assert!(oldest.finished_ms.is_some());
    assert_eq!(oldest.phase, "completed");
    let reply = submit(&mut activity, 601, 2, false);
    reply
        .send(serde_json::json!({"ok":true,"exit_code":0}))
        .unwrap();
    activity.refresh();
    assert_eq!(activity.saved.len(), 601);
    let saved = activity.saved.clone();
    let mut restored = Activity::default();
    restored.load(saved);
    assert_eq!(restored.entries().len(), 601);
    assert_eq!(restored.entries().last().unwrap().exit_code, Some(7));
}

fn deletion_fixture(id: &str, session: &str) -> AgentAuditEntry {
    AgentAuditEntry {
        id: id.into(),
        operation: id.into(),
        session: session.into(),
        session_name: session.into(),
        action: "Background command".into(),
        command: format!("fixture {id}"),
        submitted_ms: 1,
        finished_ms: Some(2),
        phase: "completed".into(),
        exit_code: Some(0),
        duration_ms: Some(1),
        detail: None,
    }
}

#[test]
fn deletion_preserves_running_new_and_unrelated_dirty_entries() {
    let ctx = egui::Context::default();
    let bridge = agent::Bridge::start(&ctx).unwrap();
    let access = bridge.mcp_access();
    assert_eq!(
        access.request(serde_json::json!({
            "action":"execute","session_id":1,"operation_id":"running",
            "command":"fixture","wait_ms":0
        }))["ok"],
        true
    );
    let request = bridge.requests.try_recv().unwrap();
    assert!(
        request
            .reply
            .start(Instant::now() + Duration::from_secs(30))
    );
    let mut activity = Activity::default();
    let mut running = deletion_fixture("running", "A");
    running.finished_ms = None;
    running.phase = "queued".into();
    activity.track(running, request.reply.clone(), request.active.clone());
    activity.retain_command(&request.reply);
    activity.refresh();
    activity.saved.extend([
        deletion_fixture("old-a", "A"),
        deletion_fixture("old-b", "B"),
        deletion_fixture("dirty-b", "B"),
    ]);
    activity.dirty.insert("dirty-b".into());
    activity.dirty.insert("old-a".into());
    activity.save_error = Some("fixture full vault".into());

    let mut viewer = Viewer::default();
    viewer.review_delete(
        "Clear Session History",
        Some("A".into()),
        activity
            .entries()
            .into_iter()
            .filter(|entry| entry.session == "A"),
    );
    let reviewed = viewer.deletion.take().unwrap().ids;
    assert_eq!(reviewed, BTreeSet::from(["old-a".into()]));
    // Arrived after confirmation: not part of the reviewed set.
    activity.saved.push_front(deletion_fixture("new-a", "A"));
    activity.dirty.insert("new-a".into());
    let before: Vec<_> = activity
        .entries()
        .into_iter()
        .map(|entry| entry.id.clone())
        .collect();
    let result = activity.delete_history(&reviewed, |_| Err("fixture write failed".into()));
    assert_eq!(result.unwrap_err(), "fixture write failed");
    assert_eq!(
        activity
            .entries()
            .into_iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        before
    );
    assert!(activity.dirty.contains("old-a"));
    activity
        .delete_history(&reviewed, |ids| {
            assert_eq!(ids, &reviewed);
            // Disk snapshot has neither unsaved new-a nor dirty-b.
            Ok(VecDeque::from([deletion_fixture("old-b", "B")]))
        })
        .unwrap();
    assert!(!activity.entries().iter().any(|entry| entry.id == "old-a"));
    assert!(activity.entries().iter().any(|entry| entry.id == "new-a"));
    assert!(activity.entries().iter().any(|entry| entry.id == "dirty-b"));
    assert!(!activity.dirty.contains("old-a"));
    assert!(activity.dirty.contains("dirty-b"));
    assert!(activity.flush_requested);
    assert!(request.authorized());
    assert!(activity.current.iter().any(|item| item.running.is_some()));
    // Clearing all cannot remove a running command, even if its ID is
    // explicitly included in a stale review.
    let mut ids: BTreeSet<_> = activity
        .entries()
        .into_iter()
        .map(|entry| entry.id.clone())
        .collect();
    ids.insert("running".into());
    activity
        .delete_history(&ids, |deleted| {
            assert!(!deleted.contains("running"));
            Ok(VecDeque::new())
        })
        .unwrap();
    assert_eq!(activity.entries().len(), 1);
    assert_eq!(activity.entries()[0].id, "running");
    request
        .reply
        .send(serde_json::json!({"ok":true,"exit_code":7}))
        .unwrap();
    activity.refresh();
    assert_eq!(activity.entries()[0].exit_code, Some(7));
    activity
        .delete_history(&ids, |_| Ok(VecDeque::new()))
        .unwrap();
    assert!(activity.entries().is_empty());
    assert!(activity.dirty.is_empty());
}

#[test]
fn live_only_deletion_does_not_require_a_vault() {
    let mut activity = Activity::default();
    activity.current.push_back(TrackedEntry {
        entry: deletion_fixture("live", "A"),
        running: None,
        save: false,
    });
    activity
        .delete_history(&BTreeSet::from(["live".into()]), |_| {
            panic!("live-only deletion must not touch disk")
        })
        .unwrap();
    assert!(activity.entries().is_empty());
}

#[test]
fn viewer_buttons_review_exact_scope_and_cancel_is_non_destructive() {
    let context = egui::Context::default();
    let theme = Theme::dark();
    let mut viewer = Viewer {
        session: Some(("A".into(), "A".into())),
        search: "one".into(),
        ..Default::default()
    };
    let mut running = deletion_fixture("active", "A");
    running.finished_ms = None;
    running.phase = "running".into();
    let entries = [
        deletion_fixture("one", "A"),
        deletion_fixture("two", "A"),
        deletion_fixture("three", "B"),
        running,
    ];
    let refs: Vec<_> = entries.iter().collect();
    // Real egui input drives each review and cancel.
    let render = |viewer: &mut Viewer, events| {
        context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        if viewer.deletion.is_some() {
                            assert!(!viewer.confirm_delete(ui, theme), "cancel must not confirm");
                        } else {
                            viewer.show(ui, theme, &refs, true, None);
                        }
                    })
                });
            },
        )
    };
    let center = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing label {label}"))
    };
    let click = |viewer: &mut Viewer, point| {
        for pressed in [true, false] {
            let _ = render(
                viewer,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    };
    for (label, expected) in [
        ("Delete Selected", vec!["one"]),
        ("Clear Session History", vec!["one", "two"]),
        ("Clear All AI History", vec!["one", "two", "three"]),
    ] {
        let output = render(&mut viewer, vec![]);
        click(&mut viewer, center(&output, label));
        assert_eq!(
            viewer.deletion.as_ref().unwrap().ids,
            expected.into_iter().map(str::to_owned).collect()
        );
        let output = render(&mut viewer, vec![]);
        click(&mut viewer, center(&output, "Cancel"));
        assert!(viewer.deletion.is_none());
    }
}
