//! Standalone, one-shot local RTerm utility. No MCP server or network listener.
#[cfg(windows)]
#[allow(dead_code)]
#[path = "../agent_ipc.rs"]
mod agent_ipc;
#[cfg(unix)]
#[allow(dead_code)]
#[path = "../agent_ipc_unix.rs"]
mod agent_ipc;

#[path = "../agent_upload.rs"]
#[allow(dead_code)]
mod agent_upload;

#[path = "../agent_protocol.rs"]
mod agent_protocol;

#[path = "rterm-agent/cli.rs"]
mod cli;

fn main() {
    match cli::run() {
        Ok(0) => (),
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("RTerm utility output error: {e}");
            std::process::exit(3);
        }
    }
}
