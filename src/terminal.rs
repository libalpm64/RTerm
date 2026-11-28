use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::ops::Range;
use std::sync::mpsc;
use std::time::Duration;

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, Term, TermMode, cell::Flags, point_to_viewport, viewport_to_point},
    vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor, Rgb},
};
use bytes::Bytes;
use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2, pos2, vec2};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};

use crate::backend::{Backend, BackendEvent, Events, InputError, next_session_id};
use crate::{highlight::Highlighter, storage::home_dir};

mod commands;
mod free_type;
mod mouse;

pub(crate) fn elapsed_label(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0 => format!("{}ms", elapsed.as_millis()),
        1..=9 => format!("{:.1}s", elapsed.as_secs_f32()),
        10..=59 => format!("{seconds}s"),
        _ => format!("{}m{:02}s", seconds / 60, seconds % 60),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Local,
    Ssh,
    Sftp,
    Telnet,
    Serial,
}

impl Kind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Local,
        Self::Ssh,
        Self::Sftp,
        Self::Telnet,
        Self::Serial,
    ];
    pub(crate) fn is_ssh(self) -> bool {
        matches!(self, Self::Ssh | Self::Sftp)
    }
    pub(crate) fn icon(self, t: crate::theme::Theme) -> (crate::theme::Icon, Color32) {
        use crate::theme::Icon;
        match self {
            Self::Local => (Icon::Terminal, t.green),
            Self::Ssh => (Icon::Server, t.blue),
            Self::Sftp => (Icon::Folder, t.yellow),
            Self::Telnet => (Icon::Network, t.purple),
            Self::Serial => (Icon::Plug, Color32::from_rgb(230, 137, 78)),
        }
    }
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Ssh => "SSH",
            Self::Sftp => "SFTP",
            Self::Telnet => "Telnet",
            Self::Serial => "Serial",
        }
    }
}

struct Size(PtySize);
impl Dimensions for Size {
    fn columns(&self) -> usize {
        self.0.cols.into()
    }
    fn screen_lines(&self) -> usize {
        self.0.rows.into()
    }
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
}

struct LocalPty {
    child: Option<Box<dyn Child + Send + Sync>>,
    input: mpsc::SyncSender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
}

impl LocalPty {
    fn spawn(
        shell: &str,
        size: PtySize,
        id: u32,
        events: Events,
        input: mpsc::SyncSender<Vec<u8>>,
        pending: mpsc::Receiver<Vec<u8>>,
    ) -> Result<Self, String> {
        let pair = portable_pty::native_pty_system()
            .openpty(size)
            .map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let candidates = if !shell.is_empty() {
            vec![shell.to_owned()]
        } else {
            default_shells()
        };
        let mut failure = String::new();
        let mut child = None;
        for candidate in candidates {
            let mut command = CommandBuilder::new(&candidate);
            command.cwd(home_dir());
            command.env("TERM", "xterm-256color");
            command.env("COLORTERM", "truecolor");
            if cfg!(windows)
                && (candidate.ends_with("powershell.exe") || candidate.ends_with("pwsh.exe"))
            {
                command.arg("-NoLogo");
            }
            match pair.slave.spawn_command(command) {
                Ok(process) => {
                    child = Some(process);
                    break;
                }
                Err(error) => failure = format!("{candidate}: {error}"),
            }
        }
        let child = child.ok_or(failure)?;
        drop(pair.slave);
        std::thread::spawn(move || {
            while let Ok(data) = pending.recv() {
                if writer
                    .write_all(&data)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => events.send(BackendEvent::Data {
                        id,
                        data: Bytes::copy_from_slice(&buffer[..count]),
                    }),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            events.send(BackendEvent::Closed(id));
        });
        Ok(Self {
            child: Some(child),
            input,
            master: pair.master,
        })
    }
}

impl Drop for LocalPty {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap the child outside the GUI thread, even when it exits after the kill.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

fn default_shells() -> Vec<String> {
    if cfg!(windows) {
        let mut shells = vec!["powershell.exe".into(), "pwsh.exe".into()];
        shells.push(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()));
        shells
    } else {
        vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            "/bin/sh".into(),
        ]
    }
}

struct LocalStartup {
    result: mpsc::Receiver<Result<LocalPty, String>>,
    input: mpsc::SyncSender<Vec<u8>>,
    size: PtySize,
}

struct TerminalEvents(mpsc::Sender<TermEvent>);
impl EventListener for TerminalEvents {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

pub(crate) fn terminal_focus_id() -> egui::Id {
    egui::Id::new("rterm-terminal-keyboard-owner")
}

pub(crate) struct Terminal {
    pub(crate) tab: u64,
    pub(crate) name: String,
    pub(crate) remote_target: Option<String>,
    pub(crate) kind: Kind,
    pub(crate) endpoint: Option<u32>,
    pub(crate) closed: bool,
    pub(crate) sftp_ready: bool,
    local: Option<LocalPty>,
    startup: Option<LocalStartup>,
    queued_local: Option<(String, bool)>,
    grid: Term<TerminalEvents>,
    parser: Processor,
    commands: commands::Commands,
    events: mpsc::Receiver<TermEvent>,
    size: PtySize,
    ssh_size: Option<PtySize>,
    fullscreen_layout: bool,
    manual_fullscreen_zoom: bool,
    rendered_font: f32,
    primary_since: Option<f64>,
    last_fullscreen_resize: Option<f64>,
    cursor: CursorStyle,
    scrollback: usize,
    input_line: String,
    pending_input: RefCell<Vec<u8>>,
    paint_scratch: PaintScratch,
    pending_user_input: Cell<bool>,
    scroll_remainder: f32,
    drag_anchor: Option<egui::Pos2>,
    cursor_activity: f64,
    was_focused: bool,
    free_type: free_type::FreeType,
    mouse: mouse::Mouse,
}

#[derive(Clone, Copy)]
pub(crate) struct TerminalOptions {
    pub(crate) font: f32,
    pub(crate) cursor: CursorStyle,
    pub(crate) scrollback: usize,
    pub(crate) copy_on_select: bool,
    pub(crate) line_numbers: bool,
    pub(crate) free_type: bool,
}

impl Terminal {
    pub(crate) fn new(tab: u64, name: String, kind: Kind, options: &TerminalOptions) -> Self {
        let size = PtySize::default();
        let (sender, events) = mpsc::channel();
        Self {
            tab,
            name,
            remote_target: None,
            kind,
            endpoint: None,
            closed: false,
            sftp_ready: false,
            local: None,
            startup: None,
            queued_local: None,
            grid: Term::new(
                terminal_config(options),
                &Size(size),
                TerminalEvents(sender),
            ),
            parser: Processor::new(),
            commands: commands::Commands::default(),
            events,
            size,
            ssh_size: None,
            fullscreen_layout: false,
            manual_fullscreen_zoom: false,
            rendered_font: options.font,
            primary_since: None,
            last_fullscreen_resize: None,
            cursor: options.cursor,
            scrollback: options.scrollback,
            input_line: String::new(),
            pending_input: RefCell::new(Vec::new()),
            paint_scratch: PaintScratch::default(),
            pending_user_input: Cell::new(false),
            scroll_remainder: 0.0,
            drag_anchor: None,
            cursor_activity: 0.0,
            was_focused: false,
            free_type: Default::default(),
            mouse: Default::default(),
        }
    }

    pub(crate) fn start_local(&mut self, shell: &str, backend: &Backend) -> Result<(), String> {
        let id = next_session_id();
        let shell = shell.to_owned();
        let size = self.size;
        let events = backend.events.clone();
        let (input, pending) = mpsc::sync_channel(128);
        let worker_input = input.clone();
        let (sender, result) = mpsc::channel();
        std::thread::Builder::new()
            .name("rterm-shell-start".into())
            .spawn(move || {
                let result = LocalPty::spawn(&shell, size, id, events, worker_input, pending);
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.startup = Some(LocalStartup {
            result,
            input,
            size,
        });
        self.endpoint = Some(id);
        Ok(())
    }

    pub(crate) fn starting(&self) -> bool {
        !self.closed
            && (self.startup.is_some()
                || self.queued_local.is_some()
                || (self.kind == Kind::Ssh && self.ssh_size.is_none())
                || (self.kind == Kind::Sftp && !self.sftp_ready)
                || (self.kind != Kind::Local && self.endpoint.is_none()))
    }

    // These local checks cannot tell whether the remote shell is at a prompt.
    pub(crate) fn agent_prompt_available(&self) -> bool {
        self.kind == Kind::Ssh
            && !self.closed
            && !self.starting()
            && !self.grid.mode().contains(TermMode::ALT_SCREEN)
            && self.pending_input.borrow().is_empty()
            && self.input_line.is_empty()
    }

    pub(crate) fn launching(&self) -> bool {
        self.startup.is_some()
    }

    pub(crate) fn queue_local(&mut self, shell: &str, banner: bool) {
        self.queued_local = Some((shell.to_owned(), banner));
    }

    pub(crate) fn prepare_local(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        options: &TerminalOptions,
        backend: &Backend,
    ) -> Result<(), String> {
        let Some((shell, banner)) = self.queued_local.take() else {
            return Ok(());
        };
        // Construct the empty grid at its real dimensions, rather than reflow
        // a default 80x24 grid or let ConPTY redraw at the wrong initial size.
        self.reset_empty_grid(
            terminal_viewport(ui, rect, options, false, false).size,
            options,
        );
        if banner {
            self.receive(b"\x1b[1;32mLocal Terminal\x1b[0m\r\n");
        }
        self.start_local(&shell, backend)
    }

    fn reset_empty_grid(&mut self, size: PtySize, options: &TerminalOptions) {
        self.size = size;
        let (sender, events) = mpsc::channel();
        self.grid = Term::new(
            terminal_config(options),
            &Size(self.size),
            TerminalEvents(sender),
        );
        self.events = events;
        self.parser = Processor::new();
    }

    pub(crate) fn prepare_remote(&mut self, ui: &egui::Ui, rect: Rect, options: &TerminalOptions) {
        if self.kind == Kind::Local || self.closed || self.endpoint.is_some() {
            return;
        }
        let size = terminal_viewport(ui, rect, options, false, false).size;
        if size != self.size {
            self.reset_empty_grid(size, options);
        }
    }

    pub(crate) fn poll_local(&mut self) -> Option<Result<(), String>> {
        let result = match self.startup.as_ref()?.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("Shell startup worker stopped".into()),
        };
        let initial_size = self.startup.take().unwrap().size;
        Some(result.and_then(|pty| {
            let resize = if initial_size != self.size {
                pty.master
                    .resize(self.size)
                    .map_err(|error| error.to_string())
            } else {
                Ok(())
            };
            self.local = Some(pty);
            resize
        }))
    }

    pub(crate) fn mark_closed(&mut self) {
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.parser.stop_sync(&mut self.grid);
        }
        self.finish_shell_command(None);
        self.closed = true;
        self.sftp_ready = false;
        self.free_type = Default::default();
        self.mouse = Default::default();
        self.ssh_size = None;
        self.pending_input.get_mut().clear();
        self.pending_user_input.set(false);
        self.startup = None;
        self.queued_local = None;
        if let Some(pty) = self.local.take() {
            std::thread::spawn(move || drop(pty));
        }
    }

    pub(crate) fn begin_ssh_reconnect(&mut self) {
        self.mark_closed();
        self.endpoint = None;
        self.input_line.clear();
        self.parser = Processor::new();
        self.commands.reset_parser();
        self.receive(b"\x1b[?1049l\x1b[0m\r\n[Reconnecting SSH session]\r\n");
        self.closed = false;
    }

    // Check the terminal grid, so the benchmark ends after the application has parsed the payload.
    #[cfg(any(test, feature = "benchmark"))]
    pub(crate) fn visible_line_is(&self, text: &str) -> bool {
        self.visible_line_matches(|line| line == text)
    }

    #[cfg(any(test, feature = "benchmark"))]
    pub(crate) fn visible_line_matches(&self, matches: impl Fn(&str) -> bool) -> bool {
        let grid = self.grid.grid();
        (0..self.size.rows).any(|line| {
            let row = &grid[alacritty_terminal::index::Line(i32::from(line))];
            let value = (0..self.size.cols)
                .map(|column| row[Column(usize::from(column))].c)
                .collect::<String>();
            matches(value.trim_end())
        })
    }

    pub(crate) fn receive(&mut self, data: &[u8]) {
        self.flush_sync_timeout();
        self.receive_command_markers(data);
    }

    pub(crate) fn service(&mut self, backend: &Backend, context: &egui::Context, bell: bool) {
        self.flush_sync_timeout();
        if let Some(deadline) = self.parser.sync_timeout().sync_timeout() {
            context.request_repaint_after(
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        self.sync_size(backend);
        self.flush_pending_input(backend);
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::PtyWrite(text) => {
                    let _ = self.enqueue_typed_input(backend, text.into_bytes(), true);
                }
                TermEvent::ClipboardStore(_, text) => context.copy_text(text),
                TermEvent::ColorRequest(index, formatter) => {
                    let color = self.grid.colors()[index]
                        .map(rgb)
                        .unwrap_or_else(|| palette_color(index));
                    let _ = self.enqueue_typed_input(
                        backend,
                        formatter(Rgb {
                            r: color.r(),
                            g: color.g(),
                            b: color.b(),
                        })
                        .into_bytes(),
                        true,
                    );
                }
                TermEvent::TextAreaSizeRequest(formatter) => {
                    let _ = self.enqueue_typed_input(
                        backend,
                        formatter(WindowSize {
                            num_lines: self.size.rows,
                            num_cols: self.size.cols,
                            cell_width: self.size.pixel_width / self.size.cols,
                            cell_height: self.size.pixel_height / self.size.rows,
                        })
                        .into_bytes(),
                        true,
                    );
                }
                TermEvent::Bell if bell => {
                    context.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ))
                }
                _ => {}
            }
        }
    }

    pub(crate) fn send(&self, backend: &Backend, data: Vec<u8>) -> bool {
        self.enqueue_input(backend, data).is_ok()
    }

    fn enqueue_input(&self, backend: &Backend, data: Vec<u8>) -> Result<(), &'static str> {
        self.enqueue_typed_input(backend, data, false)
    }

    fn enqueue_typed_input(
        &self,
        backend: &Backend,
        data: Vec<u8>,
        automatic: bool,
    ) -> Result<(), &'static str> {
        if self.kind == Kind::Sftp {
            return Err("SFTP-only session has no shell");
        }
        if self.closed {
            return Err("The terminal session is closed");
        }
        // Retain short bursts during transport backpressure, in order, without
        // blocking the GUI or allocating an unbounded queue of key events.
        const MAX_PENDING: usize = 1024 * 1024;
        let mut pending = self.pending_input.borrow_mut();
        if !pending.is_empty()
            || (self.kind == Kind::Ssh && self.endpoint.is_some() && self.ssh_size.is_none())
        {
            if pending.len().saturating_add(data.len()) > MAX_PENDING {
                return Err("Terminal input buffer is full; input was not sent");
            }
            pending.extend(data);
            self.pending_user_input
                .set(self.pending_user_input.get() || !automatic);
            backend.schedule_input_retry();
            return Ok(());
        }
        match self.try_send_input(backend, data, automatic) {
            Ok(()) => Ok(()),
            Err(InputError::Full(data)) if data.len() <= MAX_PENDING => {
                *pending = data;
                self.pending_user_input.set(!automatic);
                backend.schedule_input_retry();
                Ok(())
            }
            Err(InputError::Full(_)) => Err("Terminal input buffer is full; input was not sent"),
            Err(InputError::Closed) => {
                Err("Terminal input channel is unavailable; input was not sent")
            }
        }
    }

    fn flush_pending_input(&mut self, backend: &Backend) {
        if self.kind == Kind::Ssh && self.ssh_size.is_none() {
            // The SSH authentication reply assigns an endpoint before opening
            // the PTY. Keep input until the shell reply confirms its writer.
            return;
        }
        let data = std::mem::take(self.pending_input.get_mut());
        let automatic = !self.pending_user_input.replace(false);
        if data.is_empty() || self.closed {
            return;
        }
        match self.try_send_input(backend, data, automatic) {
            Ok(()) => {}
            Err(InputError::Full(data)) => {
                *self.pending_input.get_mut() = data;
                self.pending_user_input.set(!automatic);
                backend.schedule_input_retry();
            }
            Err(InputError::Closed) => {
                self.receive(
                    b"\r\n\x1b[90m[Input channel closed; buffered input was not sent]\x1b[0m\r\n",
                );
            }
        }
    }

    fn try_send_input(
        &self,
        backend: &Backend,
        data: Vec<u8>,
        automatic: bool,
    ) -> Result<(), InputError> {
        if let Some(input) = self
            .local
            .as_ref()
            .map(|local| &local.input)
            .or_else(|| self.startup.as_ref().map(|startup| &startup.input))
        {
            match input.try_send(data) {
                Ok(()) => Ok(()),
                Err(mpsc::TrySendError::Full(data)) => Err(InputError::Full(data)),
                Err(mpsc::TrySendError::Disconnected(_)) => Err(InputError::Closed),
            }
        } else if self.kind == Kind::Local {
            Err(InputError::Closed)
        } else if automatic {
            backend.write_terminal_reply(self.endpoint.ok_or(InputError::Closed)?, data)
        } else {
            backend.write(self.endpoint.ok_or(InputError::Closed)?, data)
        }
    }

    pub(crate) fn paste(&self, backend: &Backend, text: &str) -> bool {
        self.send(backend, self.paste_bytes(text))
    }

    fn paste_bytes(&self, text: &str) -> Vec<u8> {
        if self.grid.mode().contains(TermMode::BRACKETED_PASTE) {
            let text = text.replace(['\x1b', '\x03'], "");
            format!("\x1b[200~{text}\x1b[201~").into_bytes()
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        }
    }

    pub(crate) fn copy(&self, context: &egui::Context) {
        if let Some(text) = self.grid.selection_to_string() {
            context.copy_text(text);
        }
    }

    pub(crate) fn select_all(&mut self) {
        let start = Point::new(self.grid.topmost_line(), Column(0));
        let end = Point::new(self.grid.bottommost_line(), self.grid.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        self.grid.selection = Some(selection);
    }

    pub(crate) fn clear(&mut self) {
        self.receive(b"\x1b[2J\x1b[3J\x1b[H");
        self.reset_command_selection();
    }

    pub(crate) fn status(&self) -> (String, String) {
        let point = self.grid.grid().cursor.point;
        (
            format!("{}x{}", self.size.cols, self.size.rows),
            format!("Ln {}, Col {}", point.line.0 + 1, point.column.0 + 1),
        )
    }

    pub(crate) fn size(&self) -> PtySize {
        self.size
    }

    pub(crate) fn ssh_ready(&mut self, backend: &Backend, initial_size: PtySize) {
        self.ssh_size = Some(initial_size);
        self.sync_size(backend);
        self.flush_pending_input(backend);
    }

    fn sync_size(&mut self, backend: &Backend) {
        if self.closed
            || self.kind != Kind::Ssh
            || self.ssh_size.is_none()
            || self.ssh_size == Some(self.size)
        {
            return;
        }
        if let Some(id) = self.endpoint {
            if backend.resize(id, self.size) {
                self.ssh_size = Some(self.size);
            } else {
                // Never lose the last viewport size when the input queue is full.
                backend.schedule_input_retry();
            }
        }
    }

    pub(crate) fn zoom_base(&self, configured: f32) -> f32 {
        if self.fullscreen_layout && !self.manual_fullscreen_zoom {
            self.rendered_font
        } else {
            configured
        }
    }

    pub(crate) fn manual_zoom(&mut self) {
        if self.fullscreen_layout || self.grid.mode().contains(TermMode::ALT_SCREEN) {
            self.manual_fullscreen_zoom = true;
        }
    }

    fn update_fullscreen_layout(&mut self, ui: &egui::Ui) -> bool {
        const EXIT_GRACE: f64 = 0.40;
        let now = ui.input(|input| input.time);
        if self.grid.mode().contains(TermMode::ALT_SCREEN) {
            self.fullscreen_layout = true;
            self.primary_since = None;
        } else if self.fullscreen_layout {
            // Normal exits should restore instantly. Only retain the alternate
            // buffer if a recent resize could have triggered a curses redraw.
            let recent_resize = self
                .last_fullscreen_resize
                .is_some_and(|time| now - time < 1.0);
            let remaining = if recent_resize || self.primary_since.is_some() {
                EXIT_GRACE - (now - *self.primary_since.get_or_insert(now))
            } else {
                0.0
            };
            if remaining <= 0.0 || self.closed {
                self.fullscreen_layout = false;
                self.manual_fullscreen_zoom = false;
                self.primary_since = None;
            } else {
                // ncurses may temporarily leave the alternate screen during
                // SIGWINCH. Do not send a second, contradictory PTY resize.
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(remaining));
            }
        }
        self.fullscreen_layout
    }

    // Keep the render dependencies explicit at the pane boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        backend: &Backend,
        options: &TerminalOptions,
        highlighter: &Highlighter,
        allow_input: bool,
        allow_pointer: bool,
        request_focus: bool,
        preview_font: Option<f32>,
        wallpaper: Option<&crate::background::Wallpaper>,
    ) -> (egui::Response, Vec<String>, Option<String>) {
        let mut error = None;
        let id = ui.id().with(("terminal", self.tab));
        let viewport = rect;
        if let Err(failure) = self.prepare_local(ui, viewport, options, backend) {
            self.mark_closed();
            self.receive(format!("Could not start the shell: {failure}\r\n").as_bytes());
            error = Some(failure);
        }
        let fullscreen = self.update_fullscreen_layout(ui);
        let layout = terminal_viewport(
            ui,
            viewport,
            options,
            fullscreen,
            !self.manual_fullscreen_zoom,
        );
        self.rendered_font = layout.font.size;
        let committed_size = layout.size;
        // Paint each wheel step at its requested font immediately, while the
        // grid and PTY retain the last committed size. Only the settled zoom
        // reflows history or sends a resize to the shell.
        let layout = preview_font.map_or(layout, |font| {
            terminal_viewport(
                ui,
                viewport,
                &TerminalOptions { font, ..*options },
                fullscreen,
                false,
            )
        });
        let TerminalViewport {
            rect,
            cell,
            font,
            gutter,
            scrollbar,
            size: visible_size,
        } = layout;
        let response = ui.interact(
            viewport,
            id,
            if allow_pointer {
                if allow_input {
                    Sense::click_and_drag()
                } else {
                    Sense::CLICK | Sense::DRAG
                }
            } else {
                Sense::hover()
            },
        );
        let size = committed_size;
        if size != self.size {
            if size.cols != self.size.cols || size.rows != self.size.rows {
                self.grid.resize(Size(size));
                if fullscreen {
                    self.last_fullscreen_resize = Some(ui.input(|input| input.time));
                }
            }
            self.size = size;
            if let Some(pty) = &self.local {
                if let Err(failure) = pty.master.resize(size) {
                    error = Some(failure.to_string());
                }
            } else if self.kind != Kind::Local {
                self.sync_size(backend);
            }
        }
        if options.cursor != self.cursor || options.scrollback != self.scrollback {
            self.grid.set_options(terminal_config(options));
            self.cursor = options.cursor;
            self.scrollback = options.scrollback;
        }
        // During a preview, keep the cursor on-screen if the old grid is
        // larger than the newly scaled viewport. This is purely a paint offset.
        let (first_row, first_column) = if preview_font.is_some()
            && self.grid.grid().display_offset() == 0
        {
            let cursor = self.grid.grid().cursor.point;
            (
                ((cursor.line.0 + 1).max(0) as usize).saturating_sub(visible_size.rows as usize),
                (cursor.column.0 + 1).saturating_sub(visible_size.cols as usize),
            )
        } else {
            (0usize, 0usize)
        };
        let preview_offset = vec2(first_column as f32 * cell.x, first_row as f32 * cell.y);
        if allow_pointer && preview_font.is_none() {
            if !self.application_mouse(ui, &response, rect, cell, backend)
                && (!fullscreen || self.grid.mode().contains(TermMode::ALT_SCREEN))
            {
                self.pointer(ui, &response, rect, cell, options, backend);
            }
        } else {
            self.mouse.suppress_menu = false;
            self.free_cancel_drag();
            self.drag_anchor = None;
        }
        // Paint one continuous image across the pane, including the scroll
        // track. Keep the grid and zoom-preview geometry independent of it.
        let wallpaper = wallpaper.filter(|wallpaper| {
            wallpaper.visible()
                && self.grid.colors()[NamedColor::Background]
                    .map(rgb)
                    .unwrap_or(Color32::BLACK)
                    == Color32::BLACK
        });
        if wallpaper.is_none() {
            let bg = self.grid.colors()[NamedColor::Background]
                .map(rgb)
                .unwrap_or(Color32::BLACK);
            ui.painter().rect_filled(viewport, 0.0, bg);
        }
        if let Some(wallpaper) = wallpaper {
            let painter = ui.painter().with_clip_rect(viewport);
            painter.rect_filled(viewport, 0.0, Color32::BLACK);
            wallpaper.paint(&painter, viewport);
        }
        if let Some(scrollbar) = scrollbar {
            self.scrollbar(
                ui,
                scrollbar,
                allow_pointer && preview_font.is_none(),
                wallpaper.is_some(),
            );
        }
        // Register the terminal and its scrollbar before requesting focus. Egui
        // can otherwise surrender focus on a click outside the text rectangle
        // (e.g. a pane header, line-number gutter, or scroll track).
        if allow_input && (request_focus || response.clicked() || response.drag_started()) {
            response.request_focus();
        }
        let focused = allow_input && response.has_focus();
        if focused
            && (!self.was_focused || request_focus || ui.input(|input| !input.events.is_empty()))
        {
            self.cursor_activity = ui.input(|input| input.time);
        }
        self.was_focused = focused;
        let mut history = Vec::new();
        if focused {
            // The eframe raw-input hook uses this marker to keep terminal
            // navigation out of egui's begin-pass focus traversal.
            ui.ctx()
                .data_mut(|data| data.insert_temp(terminal_focus_id(), (id, self.tab)));
            if response.gained_focus() {
                ui.ctx().request_repaint();
            }
            // These keys belong to the shell (completion, history, cursor
            // movement, escape), not egui's directional focus navigation.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
            let point = self.grid.grid().cursor.point;
            let origin = rect.min
                + vec2(point.column.0 as f32 * cell.x, point.line.0 as f32 * cell.y)
                - preview_offset;
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    rect,
                    cursor_rect: Rect::from_min_size(origin, cell),
                });
            });
            let events = ui.input(|input| input.events.clone());
            // Keep the event order, but send all input from this frame together.
            // A burst of Text/IME events must not fill one channel slot per character.
            let mut input_bytes = Vec::new();
            let mut caret = if options.free_type
                && self.grid.mode().contains(TermMode::BRACKETED_PASTE)
                && !self
                    .grid
                    .mode()
                    .intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
            {
                self.free_type
                    .caret
                    .or_else(|| self.editable_line().map(|line| line.caret))
                    .filter(|&i| {
                        i <= self.input_line.len() && free_type::plain_text(&self.input_line)
                    })
            } else {
                None
            };
            let mut replace_selection = options.free_type && self.grid.selection.is_some();
            for event in events {
                match event {
                    egui::Event::Copy => {
                        if self.grid.selection_to_string().is_some() {
                            self.copy(ui.ctx());
                        } else if ui.input(|input| input.modifiers.ctrl && !input.modifiers.shift) {
                            self.input_line.clear();
                            caret = None;
                            input_bytes.push(3);
                        }
                    }
                    egui::Event::Cut
                        if ui.input(|input| input.modifiers.ctrl && !input.modifiers.shift) =>
                    {
                        self.input_line.clear();
                        caret = None;
                        input_bytes.push(24);
                    }
                    egui::Event::Paste(text) => {
                        if options.free_type && free_type::plain_text(&text) {
                            if replace_selection {
                                caret = self.free_delete_selection(&mut input_bytes).or(caret);
                                replace_selection = false;
                            }
                            if let Some(at) = caret {
                                self.input_line.insert_str(at, &text);
                                caret = Some(at + text.len());
                            } else if self.grid.mode().contains(TermMode::BRACKETED_PASTE) {
                                self.input_line.push_str(&text);
                            }
                            self.input_line
                                .truncate(self.input_line.floor_char_boundary(4096));
                            caret = caret.filter(|&at| at <= self.input_line.len());
                        } else {
                            caret = None;
                        }
                        input_bytes.extend(self.paste_bytes(&text));
                    }
                    egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text))
                        if !text.is_empty() =>
                    {
                        if replace_selection && free_type::plain_text(&text) {
                            caret = self.free_delete_selection(&mut input_bytes).or(caret);
                            replace_selection = false;
                        }
                        if let Some(at) = caret.filter(|_| free_type::plain_text(&text)) {
                            self.input_line.insert_str(at, &text);
                            caret = Some(at + text.len());
                        } else {
                            self.input_line.push_str(&text);
                            caret = None;
                        }
                        self.input_line
                            .truncate(self.input_line.floor_char_boundary(4096));
                        caret = caret.filter(|&at| at <= self.input_line.len());
                        input_bytes.extend(text.into_bytes());
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if replace_selection
                            && matches!(key, Key::Backspace | Key::Delete)
                            && !modifiers.ctrl
                            && !modifiers.alt
                        {
                            replace_selection = false;
                            if let Some(at) = self.free_delete_selection(&mut input_bytes) {
                                caret = Some(at);
                                continue;
                            }
                        }
                        if key_bytes(key, modifiers, *self.grid.mode(), self.kind).is_some() {
                            replace_selection = false;
                        }
                        if key == Key::Enter {
                            // Only retain text echoed by the shell. Hidden password input stays out of history.
                            if !self.input_line.trim().is_empty() {
                                let point = self.grid.grid().cursor.point;
                                let row = &self.grid.grid()[point.line];
                                let visible = (0..point.column.0)
                                    .map(|col| row[Column(col)].c)
                                    .collect::<String>();
                                if visible.ends_with(&self.input_line)
                                    || self.editable_line().is_some()
                                {
                                    history.push(self.input_line.trim().to_owned());
                                }
                            }
                            self.input_line.clear();
                            caret = None;
                        } else if key == Key::Backspace && !modifiers.ctrl && !modifiers.alt {
                            if let Some(at) = caret {
                                if at > 0 {
                                    self.input_line.remove(at - 1);
                                    caret = Some(at - 1);
                                }
                            } else {
                                self.input_line.pop();
                            }
                        } else if key == Key::Delete && !modifiers.ctrl && !modifiers.alt {
                            if let Some(at) = caret
                                && at < self.input_line.len()
                            {
                                self.input_line.remove(at);
                            }
                        } else if !modifiers.ctrl && !modifiers.alt && !modifiers.shift {
                            caret = caret.map(|at| match key {
                                Key::ArrowLeft => at.saturating_sub(1),
                                Key::ArrowRight => (at + 1).min(self.input_line.len()),
                                Key::Home => 0,
                                Key::End => self.input_line.len(),
                                _ => at,
                            });
                            if matches!(
                                key,
                                Key::Tab | Key::Escape | Key::ArrowUp | Key::ArrowDown | Key::Enter
                            ) {
                                caret = None;
                                if matches!(key, Key::ArrowUp | Key::ArrowDown) {
                                    self.input_line.clear();
                                }
                            }
                        } else if !matches!(key, Key::Space)
                            && (modifiers.ctrl || matches!(key, Key::ArrowUp | Key::ArrowDown))
                        {
                            self.input_line.clear();
                            caret = None;
                        } else if modifiers.alt || modifiers.shift {
                            caret = None;
                        }
                        if let Some(bytes) = key_bytes(key, modifiers, *self.grid.mode(), self.kind)
                        {
                            input_bytes.extend(bytes);
                        }
                    }
                    _ => {}
                }
            }
            self.input_line
                .truncate(self.input_line.floor_char_boundary(4096));
            self.free_type.caret = caret.filter(|&at| at <= self.input_line.len());
            if !input_bytes.is_empty() {
                self.reset_command_selection();
                self.input(backend, input_bytes, &mut error);
            }
        }
        // Match the grid's background and current-line band in the margin;
        // a black gap between the gutter and grid looked like a stray border.
        let margin = rect.with_min_x(gutter.map_or(viewport.left(), |r| r.right()));
        let margin = margin.with_max_x(rect.left());
        let grid = self.grid.grid();
        if focused && !fullscreen && grid.display_offset() == 0 && !self.closed {
            let y = rect.top() + grid.cursor.point.line.0 as f32 * cell.y - preview_offset.y;
            ui.painter().with_clip_rect(margin).rect_filled(
                Rect::from_min_size(pos2(margin.left(), y), vec2(margin.width(), cell.y)),
                0.0,
                if wallpaper.is_some() {
                    Color32::from_rgba_unmultiplied(21, 21, 29, 140)
                } else {
                    super::theme::color(0x15151d)
                },
            );
        }
        if let Some(gutter) = gutter {
            self.paint_line_numbers(ui, gutter, cell.y, font.size, first_row, focused);
        } else if !fullscreen {
            let left = rect.left() - 7.0;
            let offset = self.grid.grid().display_offset() as i32;
            for row in first_row..usize::from(self.size.rows) {
                let top = rect.top() + (row - first_row) as f32 * cell.y;
                let area = Rect::from_min_size(pos2(left, top), vec2(7.0, cell.y));
                self.paint_command_mark(ui, area, Line(row as i32 - offset));
            }
        }
        self.paint(
            ui,
            rect,
            cell,
            &font,
            highlighter,
            focused,
            preview_offset,
            wallpaper.is_some(),
        );
        (response, history, error)
    }

    fn scrollbar(&mut self, ui: &egui::Ui, rect: Rect, allow_input: bool, wallpaper: bool) {
        let grid = self.grid.grid();
        let history = grid.history_size();
        let offset = grid.display_offset();
        let height = rect.height();
        let thumb = (height * f32::from(self.size.rows)
            / (history + usize::from(self.size.rows)).max(1) as f32)
            .clamp(20.0_f32.min(height), height);
        let travel = height - thumb;
        let response = ui.interact(
            rect,
            ui.id().with(("terminal-scrollbar", self.tab)),
            if allow_input {
                Sense::CLICK | Sense::DRAG
            } else {
                Sense::hover()
            },
        );
        if allow_input
            && history > 0
            && travel > 0.0
            && (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let fraction = ((pointer.y - rect.top() - thumb * 0.5) / travel).clamp(0.0, 1.0);
            let desired = ((1.0 - fraction) * history as f32).round() as i32;
            self.reset_command_selection();
            self.grid
                .scroll_display(Scroll::Delta(desired - offset as i32));
        }
        if !wallpaper {
            ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        }
        if history > 0 {
            let top = rect.top()
                + travel * (1.0 - self.grid.grid().display_offset() as f32 / history as f32);
            ui.painter().rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + 1.0, top),
                    vec2(rect.width() - 1.0, thumb),
                ),
                0.0,
                super::theme::color(if response.hovered() || response.dragged() {
                    0x666666
                } else {
                    0x444444
                }),
            );
        }
    }

    fn paint_line_numbers(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        row_height: f32,
        font_size: f32,
        first_row: usize,
        focused: bool,
    ) {
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        painter.vline(
            rect.right() - 0.5,
            rect.y_range(),
            Stroke::new(1.0_f32, super::theme::color(0x282828)),
        );
        if self.grid.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let font = FontId::monospace(font_size.max(8.0));
        let grid = self.grid.grid();
        let first_number = grid.history_size() - grid.display_offset() + 1;
        let cursor = grid.cursor.point.line;
        for row in first_row as u16..self.size.rows {
            let y = f32::from(row) - first_row as f32;
            let line = Line(i32::from(row) - grid.display_offset() as i32);
            if line > cursor && grid[line].is_clear() {
                continue;
            }
            if focused && line == cursor && grid.display_offset() == 0 {
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(rect.left(), rect.top() + y * row_height),
                        vec2(rect.width() - 1.0, row_height),
                    ),
                    0.0,
                    super::theme::color(0x15151d),
                );
            }
            let badge = Rect::from_min_size(
                pos2(rect.left(), rect.top() + y * row_height),
                vec2(rect.width() - 1.0, row_height),
            );
            if self.paint_command_mark(ui, badge, line) {
                continue;
            }
            painter.text(
                pos2(rect.right() - 8.0, rect.top() + (y + 0.5) * row_height),
                Align2::RIGHT_CENTER,
                (first_number + usize::from(row)).to_string(),
                font.clone(),
                if focused && line == cursor {
                    super::theme::color(0xff008f)
                } else {
                    super::theme::color(0x2b91af)
                },
            );
        }
    }

    fn input(&mut self, backend: &Backend, bytes: Vec<u8>, error: &mut Option<String>) {
        match self.enqueue_input(backend, bytes) {
            Ok(()) => {
                self.grid.scroll_display(Scroll::Bottom);
                self.grid.selection = None;
            }
            Err(reason) if !self.closed && self.endpoint.is_some() => *error = Some(reason.into()),
            Err(_) => {}
        }
    }

    fn pointer(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        cell: Vec2,
        options: &TerminalOptions,
        backend: &Backend,
    ) {
        let copy_on_select = options.copy_on_select;
        let free_type = options.free_type && !ui.input(|input| input.modifiers.shift);
        let over_grid = response.hovered() && ui.rect_contains_pointer(rect);
        if over_grid {
            // The terminal already has line-based scrolling. Egui's generic
            // easing spreads a wheel notch across several frames and makes SSH
            // scrollback feel delayed even though the history is entirely local.
            self.scroll_remainder += ui.input(|input| input.raw_scroll_delta.y) / cell.y;
            let lines = self.scroll_remainder.trunc() as i32;
            if lines != 0 {
                let mode = *self.grid.mode();
                if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                    let key = if lines.is_positive() {
                        Key::ArrowUp
                    } else {
                        Key::ArrowDown
                    };
                    if let Some(sequence) = key_bytes(key, egui::Modifiers::NONE, mode, self.kind) {
                        self.send(
                            backend,
                            sequence.repeat(lines.unsigned_abs().min(32) as usize),
                        );
                    }
                } else {
                    self.reset_command_selection();
                    self.grid.scroll_display(Scroll::Delta(lines));
                }
                self.scroll_remainder -= lines as f32;
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let size = self.size;
        let offset = self.grid.grid().display_offset();
        let locate = |position: egui::Pos2| {
            let delta = position - rect.min;
            let row = (delta.y / cell.y)
                .floor()
                .clamp(0.0, size.rows.saturating_sub(1) as f32) as usize;
            let column = (delta.x / cell.x)
                .floor()
                .clamp(0.0, size.cols.saturating_sub(1) as f32) as usize;
            let side = if (delta.x / cell.x).fract() < 0.5 {
                Side::Left
            } else {
                Side::Right
            };
            (
                viewport_to_point(offset, Point::new(row, Column(column))),
                side,
            )
        };
        if over_grid && ui.input(|input| input.pointer.primary_pressed()) {
            self.drag_anchor = response
                .interact_pointer_pos()
                .or_else(|| ui.input(|input| input.pointer.press_origin()));
            if free_type && let Some(pos) = self.drag_anchor {
                self.free_drag_begin(locate(pos).0);
            } else {
                self.free_cancel_drag();
            }
        }
        if over_grid && (response.double_clicked() || response.triple_clicked()) {
            self.free_cancel_drag();
            if let Some(position) = response.interact_pointer_pos() {
                let (point, side) = locate(position);
                let kind = if response.triple_clicked() {
                    SelectionType::Lines
                } else {
                    SelectionType::Semantic
                };
                self.grid.selection = Some(Selection::new(kind, point, side));
                if copy_on_select {
                    self.copy(ui.ctx());
                }
            }
        } else if self.drag_anchor.is_some() && response.dragged_by(egui::PointerButton::Primary) {
            if free_type && self.free_dragging() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                return;
            }
            if let Some(position) = response.interact_pointer_pos() {
                let (point, side) = locate(position);
                if response.drag_started() || self.grid.selection.is_none() {
                    let (start, start_side) = locate(self.drag_anchor.unwrap_or(position));
                    self.grid.selection =
                        Some(Selection::new(SelectionType::Simple, start, start_side));
                }
                if let Some(selection) = &mut self.grid.selection {
                    selection.update(point, side);
                }
            }
        } else if response.drag_stopped() {
            if free_type && self.free_dragging() {
                if let Some(position) = response
                    .interact_pointer_pos()
                    .filter(|pos| rect.contains(*pos))
                {
                    let (point, side) = locate(position);
                    self.free_drag_drop(point, side, backend);
                }
                self.free_cancel_drag();
            } else if self.drag_anchor.is_some() && copy_on_select {
                self.copy(ui.ctx());
            }
            self.drag_anchor = None;
        } else if response.clicked_by(egui::PointerButton::Primary) {
            self.drag_anchor = None;
            self.free_cancel_drag();
            if over_grid {
                self.grid.selection = None;
                if free_type && let Some(position) = response.interact_pointer_pos() {
                    let (point, side) = locate(position);
                    self.free_click(point, side, backend);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        cell: Vec2,
        font: &FontId,
        highlighter: &Highlighter,
        focused: bool,
        preview_offset: Vec2,
        wallpaper: bool,
    ) {
        let painter = ui.painter().with_clip_rect(rect);
        let holding_alternate =
            self.fullscreen_layout && !self.grid.mode().contains(TermMode::ALT_SCREEN);
        let content = if holding_alternate {
            self.grid.inactive_alternate_content()
        } else {
            self.grid.renderable_content()
        };
        let default_bg = Color32::BLACK;
        let default_fg = Color32::from_gray(224);
        let bg = content.colors[NamedColor::Background]
            .map(rgb)
            .unwrap_or(default_bg);
        let cursor_color = content.colors[NamedColor::Cursor]
            .map(rgb)
            .unwrap_or(super::theme::color(0xff008f));
        if !wallpaper {
            painter.rect_filled(rect, 0.0, bg);
        }
        let active_line =
            if focused && content.display_offset == 0 && !self.fullscreen_layout && !self.closed {
                Some(content.cursor.point.line)
            } else {
                None
            };
        let line_bg = if wallpaper {
            Color32::from_rgba_unmultiplied(21, 21, 29, 140)
        } else {
            super::theme::color(0x15151d)
        };
        if let Some(line) = active_line {
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(
                        rect.left(),
                        rect.top() + line.0 as f32 * cell.y - preview_offset.y,
                    ),
                    vec2(rect.width(), cell.y),
                ),
                0.0,
                line_bg,
            );
        }
        let blink = self.grid.cursor_style().blinking && focused && !self.closed;
        let now = (ui.input(|input| input.time) - self.cursor_activity).max(0.0);
        if blink {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(0.55 - now % 0.55));
        }
        let show_cursor = !blink || ((now / 0.55) as u64).is_multiple_of(2);
        // Full-screen applications own their colors; shell semantic rules must
        // not recolor their graphs, counters, and menu text.
        let inspect_text = highlighter.is_active() && !self.fullscreen_layout;
        let PaintScratch {
            row_text,
            offsets,
            highlights,
            highlight_colors,
            run,
        } = &mut self.paint_scratch;
        if !inspect_text {
            highlight_colors.clear();
        }
        let mut line = None;
        for indexed in content.display_iter {
            if inspect_text && line != Some(indexed.point.line) {
                line = Some(indexed.point.line);
                row_text.clear();
                offsets.clear();
                let row = &self.grid.grid()[indexed.point.line];
                for column in 0..self.size.cols as usize {
                    let cell = &row[Column(column)];
                    offsets.push(row_text.len());
                    row_text.push(cell.c);
                }
                let text = row_text.trim_end();
                highlighter.ranges_into(text, highlights);
                highlight_colors.clear();
                highlight_colors.resize(self.size.cols as usize, None);
                for (range, color) in highlights.iter() {
                    let start = offsets.partition_point(|offset| *offset < range.start);
                    let end = offsets.partition_point(|offset| *offset < range.end);
                    highlight_colors[start..end].fill(Some(*color));
                }
            }
            let Some(position) = point_to_viewport(content.display_offset, indexed.point) else {
                continue;
            };
            let flags = indexed.flags;
            let origin = rect.min - preview_offset
                + vec2(
                    position.column.0 as f32 * cell.x,
                    position.line as f32 * cell.y,
                );
            let bounds = Rect::from_min_size(origin, cell);
            let mut foreground = terminal_color(
                indexed.fg,
                content.colors,
                flags.contains(Flags::BOLD),
                default_fg,
                default_bg,
            );
            if indexed.fg == Color::Named(NamedColor::Foreground)
                && let Some(Some(color)) = highlight_colors.get(indexed.point.column.0)
            {
                foreground = *color;
            }
            let mut background =
                terminal_color(indexed.bg, content.colors, false, default_fg, default_bg);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
            }
            if flags.contains(Flags::DIM) {
                foreground = foreground.gamma_multiply(0.65);
            }
            if content
                .selection
                .as_ref()
                .is_some_and(|selection| selection.contains(indexed.point))
            {
                background = Color32::from_rgb(39, 81, 101);
            }
            let is_cursor = !holding_alternate
                && indexed.point == content.cursor.point
                && !self.closed
                && show_cursor
                && content.display_offset == 0;
            if is_cursor && content.cursor.shape == CursorShape::Block && focused {
                background = cursor_color;
                foreground = bg;
            }
            // Curses applications often fill the entire alternate screen with
            // ANSI black rather than the default background. Treat that black
            // canvas as wallpaper, but retain colored and inverse cells.
            let explicit_background = indexed.bg != Color::Named(NamedColor::Background)
                && !(self.fullscreen_layout && background == default_bg);
            if background != bg
                || (active_line == Some(indexed.point.line)
                    && indexed.bg != Color::Named(NamedColor::Background))
                || (wallpaper
                    && (explicit_background
                        || flags.contains(Flags::INVERSE)
                        || (is_cursor && content.cursor.shape == CursorShape::Block && focused)))
            {
                painter.rect_filled(bounds, 0.0, background);
            }
            let visible = !flags.intersects(
                Flags::HIDDEN | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER,
            );
            let bold = flags.contains(Flags::BOLD);
            let center = pos2(origin.x, bounds.center().y);
            // JetBrains Mono has fixed ASCII advances. Group same-style ASCII
            // into a single text shape instead of laying out each cell separately.
            // Complex glyphs, decorations and the cursor keep the original path.
            if visible
                && !is_cursor
                && !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT | Flags::WIDE_CHAR)
                && indexed.zerowidth().is_none()
                && (indexed.c.is_ascii_graphic() || indexed.c == ' ')
            {
                run.push(
                    &painter,
                    font,
                    indexed.point,
                    center,
                    foreground,
                    bold,
                    indexed.c,
                );
            } else {
                run.flush(&painter, font);
                if visible && (indexed.c != ' ' || indexed.zerowidth().is_some()) {
                    let mut glyph = [0; 4];
                    let base = indexed.c.encode_utf8(&mut glyph);
                    let mut combined;
                    let text = if let Some(extra) = indexed.zerowidth() {
                        combined = String::from(&*base);
                        combined.extend(extra);
                        combined.as_str()
                    } else {
                        base
                    };
                    painter.text(center, Align2::LEFT_CENTER, text, font.clone(), foreground);
                    if bold {
                        painter.text(
                            center + vec2(0.35, 0.0),
                            Align2::LEFT_CENTER,
                            text,
                            font.clone(),
                            foreground,
                        );
                    }
                }
            }
            if flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                let y = if flags.contains(Flags::STRIKEOUT) {
                    bounds.center().y
                } else {
                    bounds.bottom() - 1.0
                };
                painter.hline(bounds.x_range(), y, Stroke::new(1.0_f32, foreground));
            }
            if is_cursor {
                match content.cursor.shape {
                    CursorShape::Block if !focused => {
                        painter.rect_stroke(
                            bounds,
                            0.0,
                            Stroke::new(1.0_f32, cursor_color.gamma_multiply(0.4)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    CursorShape::Underline => {
                        painter.hline(
                            bounds.x_range(),
                            bounds.bottom() - 1.0,
                            Stroke::new(2.0_f32, cursor_color),
                        );
                    }
                    CursorShape::Beam => {
                        painter.vline(
                            bounds.left() + 1.0,
                            bounds.y_range(),
                            Stroke::new(2.0_f32, cursor_color),
                        );
                    }
                    CursorShape::HollowBlock => {
                        painter.rect_stroke(
                            bounds,
                            0.0,
                            Stroke::new(1.0_f32, cursor_color),
                            egui::StrokeKind::Inside,
                        );
                    }
                    _ => {}
                }
            }
        }
        run.flush(&painter, font);
    }
}

struct TerminalViewport {
    rect: Rect,
    cell: Vec2,
    font: FontId,
    gutter: Option<Rect>,
    scrollbar: Option<Rect>,
    size: PtySize,
}

fn terminal_viewport(
    ui: &egui::Ui,
    viewport: Rect,
    options: &TerminalOptions,
    fullscreen: bool,
    auto_fit: bool,
) -> TerminalViewport {
    let measure = |font_size: f32| {
        let font = FontId::monospace(font_size);
        let width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        (
            font,
            vec2(width.max(1.0), ((font_size * 2.7).round() * 0.5).max(1.0)),
        )
    };
    let scrollbar = (!fullscreen).then(|| viewport.with_min_x(viewport.right() - 8.0));
    let body = scrollbar.map_or(viewport, |bar| viewport.with_max_x(bar.left()));
    let mut font_size = options.font;
    // A large shell font can leave a curses app with fewer than 40 columns.
    // Fit a conventional 80x24 viewport where practical, never enlarge text or
    // change the user's saved font, and preserve a readable minimum of 8 pt.
    if fullscreen && auto_fit {
        let (_, cell) = measure(font_size);
        let ratio = ((body.width() - 7.0) / (80.0 * cell.x))
            .min(body.height() / (24.0 * cell.y))
            .min(1.0);
        if ratio < 1.0 {
            font_size = (font_size * ratio)
                .floor()
                .clamp(8.0, options.font.max(8.0));
        }
        while font_size > 8.0 {
            let (_, cell) = measure(font_size);
            if cell.x * 80.0 <= body.width() - 7.0 && cell.y * 24.0 <= body.height() {
                break;
            }
            font_size -= 1.0;
        }
    }
    let (font, cell) = measure(font_size);
    let gutter = (!fullscreen && options.line_numbers)
        .then(|| body.with_max_x((body.left() + cell.x * 6.0).min(body.right())));
    let rect = body.with_min_x((gutter.map_or(body.left(), |g| g.right()) + 7.0).min(body.right()));
    let cols = (rect.width() / cell.x).floor().max(2.0) as u16;
    let rows = (rect.height() / cell.y).floor().max(1.0) as u16;
    let scale = ui.ctx().pixels_per_point();
    let size = PtySize {
        cols,
        rows,
        pixel_width: (f32::from(cols) * cell.x * scale).round() as u16,
        pixel_height: (f32::from(rows) * cell.y * scale).round() as u16,
    };
    TerminalViewport {
        rect,
        cell,
        font,
        gutter,
        scrollbar,
        size,
    }
}

#[derive(Default)]
struct AsciiRun {
    text: String,
    start: Option<(Point, egui::Pos2, Color32, bool)>,
}

#[derive(Default)]
struct PaintScratch {
    row_text: String,
    offsets: Vec<usize>,
    highlights: Vec<(Range<usize>, Color32)>,
    highlight_colors: Vec<Option<Color32>>,
    run: AsciiRun,
}

impl AsciiRun {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        painter: &egui::Painter,
        font: &FontId,
        point: Point,
        center: egui::Pos2,
        color: Color32,
        bold: bool,
        ch: char,
    ) {
        if self.start.is_some_and(|(start, _, fg, b)| {
            start.line != point.line
                || start.column.0 + self.text.len() != point.column.0
                || fg != color
                || b != bold
        }) {
            self.flush(painter, font);
        }
        if self.start.is_none() {
            if ch == ' ' {
                return;
            }
            self.start = Some((point, center, color, bold));
        }
        self.text.push(ch);
    }

    fn flush(&mut self, painter: &egui::Painter, font: &FontId) {
        if let Some((_, center, color, bold)) = self.start.take() {
            let text = self.text.trim_end_matches(' ');
            painter.text(center, Align2::LEFT_CENTER, text, font.clone(), color);
            if bold {
                painter.text(
                    center + vec2(0.35, 0.0),
                    Align2::LEFT_CENTER,
                    text,
                    font.clone(),
                    color,
                );
            }
        }
        self.text.clear();
    }
}

fn terminal_config(options: &TerminalOptions) -> Config {
    Config {
        scrolling_history: options.scrollback,
        default_cursor_style: options.cursor,
        ..Config::default()
    }
}

fn key_bytes(key: Key, modifiers: egui::Modifiers, mode: TermMode, kind: Kind) -> Option<Vec<u8>> {
    if modifiers.mac_cmd {
        return None;
    }
    if modifiers.ctrl {
        let control = match key {
            Key::A => 1,
            Key::B => 2,
            Key::C => 3,
            Key::D => 4,
            Key::E => 5,
            Key::F => 6,
            Key::G => 7,
            Key::H => 8,
            Key::I => 9,
            Key::J => 10,
            Key::K => 11,
            Key::L => 12,
            Key::M => 13,
            Key::N => 14,
            Key::O => 15,
            Key::P => 16,
            Key::Q => 17,
            Key::R => 18,
            Key::S => 19,
            Key::T => 20,
            Key::U => 21,
            Key::V => 22,
            Key::W => 23,
            Key::X => 24,
            Key::Y => 25,
            Key::Z => 26,
            Key::OpenBracket => 27,
            Key::Backslash => 28,
            Key::CloseBracket => 29,
            Key::Space => 0,
            _ => 255,
        };
        if control != 255 && !(modifiers.shift && matches!(key, Key::C | Key::V)) {
            return Some(vec![control]);
        }
    }
    let bytes: &[u8] = match key {
        Key::Enter if kind == Kind::Telnet => b"\r\n",
        Key::Enter => b"\r",
        Key::Escape => b"\x1b",
        Key::Backspace => b"\x7f",
        Key::Tab if modifiers.shift => b"\x1b[Z",
        Key::Tab => b"\t",
        Key::Insert => b"\x1b[2~",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::F1 => b"\x1bOP",
        Key::F2 => b"\x1bOQ",
        Key::F3 => b"\x1bOR",
        Key::F4 => b"\x1bOS",
        Key::F5 => b"\x1b[15~",
        Key::F6 => b"\x1b[17~",
        Key::F7 => b"\x1b[18~",
        Key::F8 => b"\x1b[19~",
        Key::F9 => b"\x1b[20~",
        Key::F10 => b"\x1b[21~",
        Key::F11 => b"\x1b[23~",
        Key::F12 => b"\x1b[24~",
        Key::ArrowUp | Key::ArrowDown | Key::ArrowRight | Key::ArrowLeft | Key::Home | Key::End => {
            let suffix = match key {
                Key::ArrowUp => b'A',
                Key::ArrowDown => b'B',
                Key::ArrowRight => b'C',
                Key::ArrowLeft => b'D',
                Key::Home => b'H',
                _ => b'F',
            };
            let modifier = 1
                + u8::from(modifiers.shift)
                + 2 * u8::from(modifiers.alt)
                + 4 * u8::from(modifiers.ctrl);
            if modifier > 1 {
                return Some(format!("\x1b[1;{modifier}{}", char::from(suffix)).into_bytes());
            }
            let prefix = if mode.contains(TermMode::APP_CURSOR) {
                b'O'
            } else {
                b'['
            };
            return Some(vec![0x1b, prefix, suffix]);
        }
        _ => return None,
    };
    Some(bytes.to_vec())
}

fn rgb(color: Rgb) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

fn terminal_color(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    bold: bool,
    default_fg: Color32,
    default_bg: Color32,
) -> Color32 {
    match color {
        Color::Spec(color) => rgb(color),
        Color::Named(name) => {
            let index = if bold && (name as usize) < 8 {
                name as usize + 8
            } else {
                name as usize
            };
            colors[index].map(rgb).unwrap_or_else(|| match name {
                NamedColor::Foreground => default_fg,
                NamedColor::Background => default_bg,
                _ => palette_color(index),
            })
        }
        Color::Indexed(index) => colors[index as usize]
            .map(rgb)
            .unwrap_or_else(|| palette_color(index as usize)),
    }
}

fn palette_color(index: usize) -> Color32 {
    const ANSI: [u32; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x555555,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    match index {
        0..=15 => super::theme::color(ANSI[index]),
        16..=231 => {
            let value = index - 16;
            let channel = |part| [0, 95, 135, 175, 215, 255][part];
            Color32::from_rgb(
                channel(value / 36),
                channel(value / 6 % 6),
                channel(value % 6),
            )
        }
        232..=255 => Color32::from_gray(8 + 10 * (index - 232) as u8),
        257 => Color32::BLACK,
        258 => Color32::from_rgb(79, 195, 247),
        _ => Color32::from_gray(224),
    }
}

#[cfg(test)]
#[path = "../tests/unit/terminal.rs"]
mod tests;
