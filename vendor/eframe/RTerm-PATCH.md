# RTerm's eframe patch

Based on crates.io eframe 0.33.3, upstream commit
44cdd653e2317d300fb8a6c9c36b03f23991e803 (MIT OR Apache-2.0).
Both upstream license texts are included.

The patch adds an opt-in App::native_background_update hook called on the
native event-loop thread before waiting. Its deadline is independent of
redraw scheduling, and its window activation request is applied immediately
through winit rather than queued for a paint. Default apps do nothing.
No renderer, accessibility, clipboard, platform feature or dependency
is removed. This keeps RTerm on its existing egui API while addressing
Wayland compositors withholding redraws for hidden/minimized windows.

The Fedora live-process trace also revealed a separate presentation stall:
Mesa's `eglSwapBuffers` was waiting for a Wayland frame callback, blocking
the same native event-loop thread even though its background timer was
scheduled. Glow surfaces backed by Wayland now request swap interval zero,
and both regular and immediate viewport presentations notify winit before
submitting the buffer. Winit's compositor frame callbacks provide asynchronous
redraw pacing rather than waiting synchronously inside EGL. Other window
systems retain their configured swap interval. An explicit failure to set
the Wayland swap interval is reported rather than silently proceeding with
a blocking interval.

On a real Wayland desktop, run the ignored
`app::agent_ui::tests::native_wayland_minimized_agent_discovery` regression
in an isolated test process. It lets the fixture window map before minimizing,
continues requesting redraws, disables agent service in all paint callbacks,
and checks repeated discovery over a sustained interval. Set
`RTERM_WAYLAND_PROBE_REQUESTS=240` for a one-minute soak. It uses no
real saved profiles, SSH credentials, or production agent socket.

On Wayland, activation is a compositor-controlled xdg-activation request;
an application cannot force focus. X11/Windows/macOS restore and focus
are also subject to the window manager's policy.
