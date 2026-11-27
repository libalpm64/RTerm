use super::*;

#[test]
fn incremental_output_is_paged_and_retained_past_preview() {
    let active = AtomicBool::new(true);
    let job = Arc::new(Job::new(
        "output-test".into(),
        blake3::hash(b"output"),
        Arc::new(OutputBudget::default()),
    ));
    let reply = Reply(job.clone());
    assert!(reply.start(Instant::now() + Duration::from_secs(30)));
    let bytes = vec![b'x'; MAX_OUTPUT + 73];
    reply.capture_output(OutputStream::Stdout, &bytes);
    reply.capture_output(OutputStream::Stdout, &[0xe2, 0x82]);
    reply.capture_output(OutputStream::Stderr, b"warning");
    let first = job.snapshot_since(0, &active, Some(0));
    assert_eq!(first["output"].as_str().unwrap().len(), POLL_PAGE);
    assert_eq!(first["stdout"], first["output"]);
    assert_eq!(first["stderr"], "");
    assert_eq!(first["next_offset"], json!(POLL_PAGE));
    assert_eq!(first["status"], "running");
    assert_eq!(first["output_has_more"], true);
    let tail = job.snapshot_since(0, &active, Some(MAX_OUTPUT as u64));
    assert_eq!(tail["output"].as_str().unwrap().len(), 73);
    assert_eq!(tail["next_offset"], json!(MAX_OUTPUT + 73));
    reply.capture_output(OutputStream::Stdout, &[0xac]);
    reply.send(json!({"ok":true,"exit_code":1,"command_success":false,"stdout":"preview","truncated":true})).unwrap();
    let last = job.snapshot_since(0, &active, Some((MAX_OUTPUT + 73) as u64));
    assert_eq!(last["stdout"], "€");
    assert_eq!(last["stderr"], "warning");
    assert_eq!(last["combined_output"], false);
    assert_eq!(last["next_offset"], json!(MAX_OUTPUT + 83));
    assert_eq!(last["status"], "completed");
    assert_eq!(last["ok"], true);
    assert_eq!(last["command_success"], false);
    assert_eq!(last["output_has_more"], false);
    let path = std::path::PathBuf::from(last["output_path"].as_str().unwrap());
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[MAX_OUTPUT + 73..], b"\xe2\x82warning\xac");
    drop(job);
    assert!(!path.exists());
}

#[test]
fn output_pages_advance_when_streams_share_no_utf8_boundary() {
    let mut output = OutputLog::default();
    output.append(OutputStream::Stdout, &[0xe2]);
    output.append(OutputStream::Stderr, &vec![b'x'; POLL_PAGE]);
    output.append(OutputStream::Stdout, &[0x82, 0xac]);

    let first = output.page(0, true).unwrap();
    assert_eq!(first.next, POLL_PAGE as u64);
    assert_eq!(first.stderr.len(), POLL_PAGE - 1);
    let last = output.page(first.next, true).unwrap();
    assert_eq!(last.next, output.len);
    assert_eq!(last.stderr, "x");
}

#[test]
fn output_logs_share_an_aggregate_budget_across_registries() {
    let budget = Arc::new(OutputBudget::new(10));
    let bridge = Registry::with_budget(budget.clone());
    let mcp = Registry::with_budget(budget.clone());
    let mut first = OutputLog::new(bridge.output_budget.clone());
    let mut second = OutputLog::new(mcp.output_budget.clone());

    first.append(OutputStream::Combined, b"12345678");
    second.append(OutputStream::Combined, b"abcdefgh");
    assert_eq!(first.len, 8);
    assert_eq!(second.len, 2);
    assert!(second.truncated);
    assert_eq!(budget.used.load(Ordering::Acquire), 10);

    drop(first);
    assert_eq!(budget.used.load(Ordering::Acquire), 2);
    drop(second);
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}

#[test]
fn operation_deadlines_and_retention_are_bounded() {
    let active = Arc::new(AtomicBool::new(true));
    // Model a command that completed just after a five-minute job with
    // no connected client. The completed result is still available.
    let long = Arc::new(Job::new(
        "long-build".into(),
        blake3::hash(b"test"),
        Arc::new(OutputBudget::default()),
    ));
    assert!(Reply(long.clone()).start(Instant::now() + Duration::from_secs(600)));
    assert!(long.snapshot(0, &active)["remaining_ms"].as_u64().unwrap() > 590_000);
    Reply(long.clone())
        .send(json!({"ok":true,"stdout":"build finished"}))
        .unwrap();
    long.state.lock().unwrap().finished = Some(Instant::now() - Duration::from_secs(301));
    let mut retained = VecDeque::from([long]);
    Registry::prune(&mut retained);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].snapshot(0, &active)["stdout"], "build finished");
    let mut queued = Job::new(
        "expired-approval".into(),
        blake3::hash(b"test"),
        Arc::new(OutputBudget::default()),
    );
    queued.approval_deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(queued.snapshot(0, &active)["status"], "timed_out");
    assert!(!queued.alive.load(Ordering::Acquire));
    let running = Arc::new(Job::new(
        "expired-run".into(),
        blake3::hash(b"test"),
        Arc::new(OutputBudget::default()),
    ));
    let reply = Reply(running.clone());
    assert!(reply.start(Instant::now() - Duration::from_millis(1)));
    reply.send(error("Backend stopped the command")).unwrap();
    assert_eq!(running.snapshot(0, &active)["status"], "timed_out");
    assert!(!running.alive.load(Ordering::Acquire));
    let _ = Reply(running.clone()).send(json!({"ok":true}));
    assert_eq!(running.snapshot(0, &active)["status"], "timed_out");

    let registry = Registry::default();
    let ctx = eframe::egui::Context::default();
    let (tx, rx) = mpsc::sync_channel(1);
    for n in 0..MAX_JOBS + 3 {
        let id = format!("operation-{n}");
        let job = registry
            .submit(
                Some(id),
                Action::Execute {
                    session_id: 1,
                    command: "test".into(),
                },
                None,
                &tx,
                &active,
                &ctx,
            )
            .unwrap();
        let request = rx.try_recv().unwrap();
        request.reply.send(json!({"ok":true})).unwrap();
        assert_eq!(job.snapshot(0, &active)["status"], "completed");
    }
    assert_eq!(registry.jobs.lock().unwrap().len(), MAX_JOBS);
    assert!(registry.lookup("operation-0").is_none());
    let newest = format!("operation-{}", MAX_JOBS + 2);
    let job = registry.lookup(&newest).unwrap();
    job.state.lock().unwrap().finished = Some(Instant::now() - RESULT_TTL);
    assert!(registry.lookup(&newest).is_none());

    // Reserve capacity for cancellation even when long-poll slots fill.
    let slots: Vec<_> = (0..MAX_WAITERS)
        .map(|_| Slot::acquire(&registry.waiters, MAX_WAITERS).unwrap())
        .collect();
    let waiting = Job::new(
        "waiting".into(),
        blake3::hash(b"test"),
        Arc::new(OutputBudget::default()),
    );
    let start = Instant::now();
    assert_eq!(
        registry.wait(&waiting, MAX_WAIT_MS, &active)["status"],
        "queued"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    drop(slots);
    assert_eq!(registry.waiters.load(Ordering::Acquire), 0);
}

impl Bridge {
    pub(crate) fn test_ipc(&self, body: Value) -> std::thread::JoinHandle<Value> {
        let name = self.name.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                match crate::agent_ipc::request(
                    &name,
                    &std::env::current_exe().unwrap(),
                    &serde_json::to_vec(&body).unwrap(),
                    Duration::from_secs(40),
                )
                .await
                {
                    Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
                    Err(e) => error(format!("IPC disconnected or unavailable: {e}")),
                }
            })
        })
    }
}
