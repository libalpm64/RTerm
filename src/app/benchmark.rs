use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    models::{Connection, Profile, SshConfig},
    terminal::Terminal,
};

use super::*;

#[derive(Deserialize)]
struct BenchmarkConfig {
    transport: Transport,
    workload: Workload,
    scrollback: usize,
    #[serde(default)]
    warmups: usize,
    runs: usize,
    report: PathBuf,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Transport {
    Local {
        shell: String,
    },
    Ssh {
        host: String,
        user: String,
        key: PathBuf,
    },
    Saved {
        name: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Workload {
    Sequence {
        lines: u64,
    },
    Text {
        path: String,
        bytes: u64,
    },
    Latency {
        #[serde(default)]
        preload_lines: u64,
    },
}

impl Workload {
    fn command(&self) -> String {
        match self {
            Self::Sequence { lines } => format!("time seq 1 {lines}"),
            Self::Text { path, .. } => {
                let escaped = path.replace('\'', "'\\''");
                format!("cat -- '{escaped}'")
            }
            Self::Latency { .. } => ":".into(),
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            Self::Text { bytes, .. } => *bytes,
            Self::Latency { .. } => 0,
            Self::Sequence { lines } => {
                let mut total = *lines;
                let mut first = 1_u64;
                for digits in 1..=20 {
                    if first > *lines {
                        break;
                    }
                    let end = first.saturating_mul(10).saturating_sub(1).min(*lines);
                    total = total.saturating_add((end - first + 1).saturating_mul(digits));
                    first = first.saturating_mul(10);
                }
                total
            }
        }
    }
}

impl Transport {
    fn profile(&self) -> Option<Profile> {
        let connection = match self {
            Self::Local { shell } => Connection::Local {
                shell: shell.clone(),
            },
            Self::Ssh { host, user, key } => Connection::Ssh {
                config: SshConfig {
                    host: host.clone(),
                    user: user.clone(),
                    port: 22,
                    password: None,
                    key_path: Some(key.to_string_lossy().into_owned()),
                    key_name: None,
                    vault_pass: None,
                    name: None,
                    compression: Some(false),
                    key_passphrase: None,
                    use_agent: false,
                    keepalive: 0,
                },
                terminal: "xterm-256color".into(),
            },
            Self::Saved { .. } => return None,
        };
        Some(Profile {
            name: "Benchmark".into(),
            connection,
        })
    }
}

#[derive(Clone, Copy, Default, Serialize)]
struct Memory {
    working_set_bytes: usize,
    private_bytes: usize,
}

impl Memory {
    fn max(self, other: Self) -> Self {
        Self {
            working_set_bytes: self.working_set_bytes.max(other.working_set_bytes),
            private_bytes: self.private_bytes.max(other.private_bytes),
        }
    }
}

#[derive(Serialize)]
struct Report {
    scrollback: usize,
    payload_bytes: u64,
    idle: Option<Memory>,
    peak: Option<Memory>,
    seconds: Vec<f64>,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum Stage {
    WaitingForSavedProfile,
    WaitingForShell { started: Instant },
    WaitingForReady { sent: Instant },
    Idle { since: Instant, iteration: usize },
    Running { sent: Instant, iteration: usize },
    Finished,
}

struct Benchmark {
    app: App,
    config: BenchmarkConfig,
    report: Report,
    stage: Stage,
}

impl Benchmark {
    fn new(context: &egui::Context, config: BenchmarkConfig) -> Self {
        let saved = matches!(config.transport, Transport::Saved { .. });
        let mut settings = if saved {
            Settings::load()
        } else {
            Settings::benchmark_defaults()
        };
        settings.set("scrollback", config.scrollback.to_string());
        // The reference benchmark uses plain text with terminal color schemes disabled.
        if !saved {
            settings.set("semantic_hl", "0".into());
        }
        settings.set("lock_timeout", "0".into());
        settings.set("confirm_exit", "0".into());
        let mut app = App::with_settings(context, settings, saved);
        // The benchmark can use the normal vault unlock screen, but must not
        // persist command history or update the saved profile.
        app.persist_history = false;
        let stage = if let Some(profile) = config.transport.profile() {
            app.connect(profile, None);
            Stage::WaitingForShell {
                started: Instant::now(),
            }
        } else {
            Stage::WaitingForSavedProfile
        };
        let report = Report {
            scrollback: config.scrollback,
            payload_bytes: config.workload.bytes(),
            idle: None,
            peak: None,
            seconds: Vec::with_capacity(config.runs),
            error: None,
        };
        Self {
            app,
            config,
            report,
            stage,
        }
    }

    fn terminal(&self) -> &Terminal {
        &self.app.terminals[0]
    }

    fn step(&mut self) -> Result<bool, String> {
        const HANDSHAKE: Duration = Duration::from_secs(35);
        const WORKLOAD_LIMIT: Duration = Duration::from_secs(240);
        const READY: &str = "RTB_READY_317B";
        if matches!(self.stage, Stage::WaitingForSavedProfile) {
            if self.app.locked {
                return Ok(false);
            }
            let Transport::Saved { name } = &self.config.transport else {
                unreachable!()
            };
            let profile = self
                .app
                .profiles
                .iter()
                .find(|profile| profile.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| format!("Saved session '{name}' not found"))?;
            if profile.kind() != Kind::Ssh {
                return Err("The saved benchmark requires an SSH session".into());
            }
            let tabs: Vec<_> = self.app.terminals.iter().map(|term| term.tab).collect();
            for tab in tabs {
                self.app.close_tab(tab);
            }
            self.app.connect(profile, None);
            self.stage = Stage::WaitingForShell {
                started: Instant::now(),
            };
            return Ok(false);
        }
        if let Some(error) = &self.app.message {
            return Err(error.clone());
        }
        if self.app.terminals.is_empty() || self.terminal().closed {
            return Err("The benchmark shell closed unexpectedly".into());
        }

        match self.stage {
            Stage::WaitingForSavedProfile => unreachable!(),
            Stage::WaitingForShell { started } => {
                // Give the PTY time to exchange its cursor query before submitting a command.
                let setup = match &self.config.workload {
                    Workload::Latency { preload_lines } if *preload_lines > 0 => {
                        format!("seq 1 {preload_lines}; ")
                    }
                    _ => String::new(),
                };
                if started.elapsed() >= Duration::from_secs(1)
                    && self.terminal().send(
                        &self.app.backend,
                        format!("{setup}printf '\\n%s\\n' {READY}\r").into_bytes(),
                    )
                {
                    self.stage = Stage::WaitingForReady {
                        sent: Instant::now(),
                    };
                } else if started.elapsed() > HANDSHAKE {
                    return Err("Timed out opening the benchmark shell".into());
                }
            }
            Stage::WaitingForReady { sent } => {
                if self.terminal().visible_line_is(READY) {
                    self.stage = Stage::Idle {
                        since: Instant::now(),
                        iteration: 0,
                    };
                } else if sent.elapsed() > HANDSHAKE {
                    return Err("The shell did not display its readiness marker".into());
                }
            }
            Stage::Idle { since, iteration } if since.elapsed() >= Duration::from_millis(180) => {
                if iteration == 0 {
                    self.report.idle = process_memory();
                }
                let workload = self.config.workload.command();
                let marker = marker(iteration);
                let command = format!(
                    "{workload}; rtb_status=$?; printf '\\n%s_%s\\n' {marker} \"$rtb_status\"\r"
                );
                let sent = Instant::now();
                if !self
                    .terminal()
                    .send(&self.app.backend, command.into_bytes())
                {
                    return Err("The benchmark input queue did not accept the command".into());
                }
                self.stage = Stage::Running { sent, iteration };
            }
            Stage::Idle { .. } | Stage::Finished => {}
            Stage::Running { sent, iteration } => {
                if let Some(current) = process_memory() {
                    self.report.peak = Some(self.report.peak.unwrap_or_default().max(current));
                }
                let end = marker(iteration);
                if self.terminal().visible_line_is(&format!("{end}_0")) {
                    if let Workload::Sequence { lines } = self.config.workload
                        && !self.terminal().visible_line_is(&lines.to_string())
                    {
                        return Err(
                            "The final sequence number was missing from the terminal grid".into(),
                        );
                    }
                    if iteration >= self.config.warmups {
                        self.report.seconds.push(sent.elapsed().as_secs_f64());
                    }
                    let iteration = iteration + 1;
                    if iteration >= self.config.warmups + self.config.runs {
                        return Ok(true);
                    }
                    self.stage = Stage::Idle {
                        since: Instant::now(),
                        iteration,
                    };
                } else if self.terminal().visible_line_matches(|line| {
                    line.strip_prefix(&end)
                        .and_then(|suffix| suffix.strip_prefix('_'))
                        .and_then(|status| status.parse::<u8>().ok())
                        .is_some_and(|status| status != 0)
                }) {
                    return Err("The benchmark command exited unsuccessfully".into());
                } else if sent.elapsed() > WORKLOAD_LIMIT {
                    return Err("The benchmark workload timed out".into());
                }
            }
        }
        Ok(false)
    }
}

impl eframe::App for Benchmark {
    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        // A close request can be followed by another update before the window exits.
        if matches!(self.stage, Stage::Finished) {
            return;
        }
        self.app.update(context, frame);
        match self.step() {
            Ok(false) => {
                context.request_repaint_after(Duration::from_millis(10));
                return;
            }
            Ok(true) => {}
            Err(error) => self.report.error = Some(error),
        }
        self.stage = Stage::Finished;
        let bytes = serde_json::to_vec_pretty(&self.report).expect("serializable benchmark report");
        std::fs::write(&self.config.report, bytes).expect("write benchmark report");
        self.app.exit_confirmed = true;
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn marker(iteration: usize) -> String {
    format!("RTB_DONE_317B_{iteration:03}")
}

#[derive(Serialize)]
struct IdleReport {
    mode: String,
    before_window: Option<Memory>,
    idle: Option<Memory>,
}

struct IdleBenchmark {
    app: Option<App>,
    report: IdleReport,
    path: PathBuf,
    first_frame: Option<Instant>,
    settle: Duration,
    finished: bool,
}

impl eframe::App for IdleBenchmark {
    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(app) = &mut self.app {
            app.update(context, frame);
        } else {
            egui::CentralPanel::default().show(context, |_| {});
        }
        if self.finished {
            return;
        }
        let first_frame = self.first_frame.get_or_insert_with(Instant::now);
        if first_frame.elapsed() >= self.settle {
            self.report.idle = process_memory();
            self.finished = true;
            std::fs::write(&self.path, serde_json::to_vec_pretty(&self.report).unwrap())
                .expect("write idle benchmark report");
            if let Some(app) = &mut self.app {
                app.exit_confirmed = true;
            }
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }
}

pub(crate) fn run_idle(options: eframe::NativeOptions) -> eframe::Result {
    let before_window = process_memory();
    let options = eframe::NativeOptions {
        viewport: options.viewport.with_active(false),
        ..options
    };
    eframe::run_native(
        "Rterm Idle Benchmark",
        options,
        Box::new(move |creation| {
            let mode = std::env::args()
                .nth(2)
                .ok_or("Expected renderer, workspace, local, or windterm")?;
            let path = std::env::args_os().nth(3).ok_or("Expected a report path")?;
            let settle = std::env::args()
                .nth(4)
                .map(|value| value.parse::<u64>())
                .transpose()?
                .unwrap_or(2)
                .clamp(2, 600);
            let app = match mode.as_str() {
                "renderer" => None,
                "workspace" | "local" | "windterm" => {
                    let settings = Settings::benchmark_defaults();
                    let mut app = App::with_settings(&creation.egui_ctx, settings, false);
                    if mode != "workspace" {
                        app.new_local();
                    }
                    Some(app)
                }
                _ => return Err("Expected renderer, workspace, local, or windterm".into()),
            };
            Ok(Box::new(IdleBenchmark {
                app,
                report: IdleReport {
                    mode,
                    before_window,
                    idle: None,
                },
                path: path.into(),
                first_frame: None,
                settle: Duration::from_secs(settle),
                finished: false,
            }))
        }),
    )
}

pub(crate) fn run(options: eframe::NativeOptions) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: options.viewport.with_active(false),
        ..options
    };
    eframe::run_native(
        "Rterm Benchmark",
        options,
        Box::new(|creation| {
            let config_path = std::env::args_os()
                .nth(2)
                .ok_or("Expected a benchmark JSON config path")?;
            let config: BenchmarkConfig =
                serde_json::from_slice(&std::fs::read(Path::new(&config_path))?)?;
            if !(1..=100_000).contains(&config.scrollback)
                || config.runs == 0
                || config.runs > 20
                || config.warmups > 10
                || matches!(config.workload, Workload::Latency { preload_lines } if preload_lines > 100_000)
            {
                return Err(
                    "Use scrollback from 1 to 100,000, 1 to 20 runs, and at most 10 warmups".into(),
                );
            }
            Ok(Box::new(Benchmark::new(&creation.egui_ctx, config)))
        }),
    )
}

#[cfg(windows)]
fn process_memory() -> Option<Memory> {
    use windows_sys::Win32::System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        Threading::GetCurrentProcess,
    };
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // This uses the current process handle and a buffer sized for the requested counter structure.
    let valid = unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), counters.cb)
    };
    (valid != 0).then_some(Memory {
        working_set_bytes: counters.WorkingSetSize,
        private_bytes: counters.PrivateUsage,
    })
}

#[cfg(not(windows))]
fn process_memory() -> Option<Memory> {
    None
}
