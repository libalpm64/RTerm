# RTerm's eframe patch

Based on crates.io eframe 0.37.0, upstream commit
3c6eb8261c3293ad9c7ea1ed58487e038860ebef (MIT OR Apache-2.0).
Both upstream license texts are included.

The patch adds an opt-in App::native_background_update hook called on the
native event-loop thread before waiting. Its deadline is independent of
redraw scheduling, and its window activation request is applied immediately
through winit rather than queued for a paint. Default apps do nothing.
No renderer, accessibility, clipboard, platform feature or dependency
is removed. RTerm uses App::logic for non-painting state updates and App::ui for
rendering. The independent native wake deadline remains necessary to preserve
50 ms agent servicing when the compositor withholds redraws. The native
integration runs the mutating raw-input hook only for a real UI pass, so
logic-only ticks retain pending keyboard input instead of consuming or
replaying it.

The Fedora live-process trace also revealed a separate presentation stall:
Mesa's `eglSwapBuffers` was waiting for a Wayland frame callback, blocking
the same native event-loop thread even though its background timer was
scheduled. Upstream 0.37 already requests swap interval zero on Wayland and notifies
winit before regular presentations. RTerm retains the immediate-viewport
pre-present notification and fails explicitly if setting the swap interval
on Wayland fails. Winit's compositor frame callbacks provide asynchronous
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
