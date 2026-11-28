//! Opt-in native Wayland regression. No user profile, shell or network is used.
use eframe::egui;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::{StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    platform::wayland::EventLoopBuilderExtWayland,
    window::WindowId,
};

#[test]
#[ignore = "requires a running Wayland desktop; minimizes an isolated fixture"]
fn minimized_redraw_does_not_busy_poll_without_agent() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Probe {
        start: Instant,
        minimized: Arc<AtomicBool>,
    }
    impl eframe::App for Probe {
        fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
            let ctx = &root.ctx().clone();
            egui::CentralPanel::default().show(root, |ui| {
                ui.label("RTerm idle scheduling fixture");
            });
            if self.start.elapsed() > Duration::from_millis(700)
                && !self.minimized.swap(true, Ordering::Release)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
    struct Harness<'a> {
        native: eframe::EframeWinitApplication<'a>,
        start: Instant,
        loops: usize,
        polls: usize,
    }
    impl ApplicationHandler<eframe::UserEvent> for Harness<'_> {
        fn resumed(&mut self, e: &ActiveEventLoop) {
            self.native.resumed(e);
        }
        fn window_event(&mut self, e: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            self.native.window_event(e, id, event);
        }
        fn new_events(&mut self, e: &ActiveEventLoop, c: StartCause) {
            self.native.new_events(e, c);
        }
        fn user_event(&mut self, e: &ActiveEventLoop, event: eframe::UserEvent) {
            self.native.user_event(e, event);
        }
        fn about_to_wait(&mut self, e: &ActiveEventLoop) {
            self.native.about_to_wait(e);
            if self.start.elapsed() > Duration::from_secs(2) {
                self.loops += 1;
                if e.control_flow() == ControlFlow::Poll {
                    self.polls += 1;
                }
            }
            let end = self.start + Duration::from_secs(7);
            if Instant::now() >= end {
                e.exit();
            } else {
                // Only impose a test-wide deadline. Do not mask Poll or
                // inject periodic wakeups that would hide a scheduler bug.
                match e.control_flow() {
                    ControlFlow::Wait => e.set_control_flow(ControlFlow::WaitUntil(end)),
                    ControlFlow::WaitUntil(t) if t > end => {
                        e.set_control_flow(ControlFlow::WaitUntil(end));
                    }
                    _ => {}
                }
            }
        }
        fn exiting(&mut self, e: &ActiveEventLoop) {
            self.native.exiting(e);
        }
    }
    let mut builder = EventLoop::<eframe::UserEvent>::with_user_event();
    builder.with_wayland().with_any_thread(true);
    let event_loop = builder.build().expect("Wayland event loop");
    let minimized = Arc::new(AtomicBool::new(false));
    let probe_minimized = minimized.clone();
    let native = eframe::create_native(
        "rterm-idle-isolated-fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("RTerm idle scheduling fixture")
                .with_inner_size([400.0, 240.0]),
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Probe {
                start: Instant::now(),
                minimized: probe_minimized,
            }))
        }),
        &event_loop,
    );
    let mut harness = Harness {
        native,
        start: Instant::now(),
        loops: 0,
        polls: 0,
    };
    event_loop.run_app(&mut harness).unwrap();
    assert!(minimized.load(Ordering::Acquire));
    println!("idle_loops={} poll_loops={}", harness.loops, harness.polls);
    assert_eq!(harness.polls, 0, "must not spin waiting for redraw");
    assert!(harness.loops < 2000, "unexpected wakeup storm");
}
