#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
#[cfg(windows)]
mod agent_ipc;
#[cfg(unix)]
#[path = "agent_ipc_unix.rs"]
mod agent_ipc;
mod agent_protocol;
mod agent_upload;
mod app;
mod app_icon;
mod backend;
mod background;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod desktop_shortcut;
mod dialogs;
mod explorer;
mod highlight;
mod host_keys;
mod i18n;
mod mcp;
mod models;
mod session_import;
mod storage;
mod terminal;
mod theme;
mod tunnel;

fn main() -> eframe::Result {
    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--desktop-shortcut")) {
        let choice = std::env::args().nth(2);
        let result = match (desktop_shortcut::Setup::discover(), choice.as_deref()) {
            (Some(setup), Some("yes")) => setup.choose(true),
            (Some(setup), Some("no")) => setup.choose(false),
            _ => Err(std::io::Error::other(
                "No installed desktop shortcut target or invalid choice",
            )),
        };
        if let Err(error) = result {
            eprintln!("RTerm shortcut setup: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_app_id("rterm")
            .with_icon(app_icon::window())
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    #[cfg(feature = "benchmark")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--benchmark")) {
        return app::benchmark::run(options);
    }
    #[cfg(feature = "benchmark")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--benchmark-idle")) {
        return app::benchmark::run_idle(options);
    }

    eframe::run_native(
        "Rterm",
        options,
        Box::new(|creation| Ok(Box::new(app::App::new(&creation.egui_ctx)))),
    )
}
