use super::{
    agent_ipc,
    agent_protocol::{
        DEFAULT_WAIT_MS, MAX_COMMAND_SECS, MAX_JOBS, MAX_SCRIPT, MAX_WAIT_MS, PROTOCOL_VERSION,
        valid_id, validate_command,
    },
};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    path::Path,
    time::Duration,
};

const HELP: &str = "RTerm local agent utility\n\
Usage:\n\
  rterm-agent sessions\n\
  rterm-agent open <profile-id> <operation-id> [--wait-ms N]\n\
  rterm-agent manage <library-revision> <operation-id> <change-json|-> [--wait-ms N]\n\
  rterm-agent manage <library-revision> <operation-id> --change-file <UTF-8-json> [--wait-ms N]\n\
  rterm-agent execute <session-id> <operation-id> <command|-> [--wait-ms N] [--timeout-secs N]\n\
  rterm-agent execute <session-id> <operation-id> --command-file <UTF-8-script> [--wait-ms N] [--timeout-secs N]\n\
  rterm-agent upload <session-id> <operation-id> <absolute-local-path> <absolute-remote-path> [--wait-ms N] [--timeout-secs N]\n\
  rterm-agent poll <operation-id> [--wait-ms N] [--since BYTE_OFFSET]\n\
  rterm-agent history [--limit N]\n\
  rterm-agent cancel <operation-id>\n\
  rterm-agent --help\n\
  rterm-agent --version\n\
\n\
Prefer --command-file or - (UTF-8 stdin) for scripts; 64 KiB maximum.\n\
Library changes: create_folder (name), rename_folder (folder, name), remove_folder (folder),\n\
move_session (profile_id, folder or null), rename_session (profile_id, name), set_port (profile_id, port).\n\
Pass a JSON object with kind and the listed fields, or read it from a file/stdin. List again after each change.\n\
The utility requires RTerm's explicitly enabled AI interface.\n\
Output: one JSON object on stdout for each request. Exit 0 = accepted or successful,\n\
1 = request rejected or failed, 2 = invalid arguments, 3 = transport failure.\n\
A completed command can have a nonzero remote exit_code while ok is true.\n\
A pending response is not proof of success. Never retry an uncertain execution.\n\
All RTerm permissions and AI-client policies remain in force.";

fn operation_id(id: &str) -> Result<&str, &'static str> {
    if !valid_id(id) {
        return Err("Operation ID must be 1-64 ASCII letters, digits, hyphens or underscores");
    }
    Ok(id)
}

fn read_script(reader: impl Read) -> Result<String, &'static str> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_SCRIPT + 4) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read command input")?;
    if bytes.len()
        > MAX_SCRIPT
            + if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                3
            } else {
                0
            }
    {
        return Err("Command input exceeds 64 KiB");
    }
    let text = String::from_utf8(bytes).map_err(|_| "Command input must be UTF-8")?;
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .replace("\r\n", "\n");
    validate_command(&text)?;
    Ok(text)
}

fn open_command_file(path: &str) -> Result<std::fs::File, &'static str> {
    let path = Path::new(path);
    let link_metadata =
        std::fs::symlink_metadata(path).map_err(|_| "Cannot inspect command file")?;
    if !link_metadata.file_type().is_file() {
        return Err("Command file must be a regular file, not a symlink or special file");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if link_metadata.file_attributes() & 0x400 != 0 {
            return Err("Command file must not be a reparse point");
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK prevents a path swapped to a FIFO between the
        // metadata check and open from hanging the CLI indefinitely.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|_| "Cannot open command file")?;
    let metadata = file.metadata().map_err(|_| "Cannot inspect command file")?;
    if !metadata.is_file() {
        return Err("Command file must be a regular file");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Command file must not be a reparse point");
        }
    }
    Ok(file)
}

fn parse(args: &[String]) -> Result<Value, &'static str> {
    let mut args = args.iter().map(String::as_str);
    let action = args.next().ok_or("Missing subcommand; use --help")?;
    let mut request = match action {
        "sessions" => json!({"action":"list_sessions"}),
        "open" => json!({"action":"open_session",
                "profile_id":operation_id(args.next().ok_or("Missing profile ID")?)?,
                "operation_id":operation_id(args.next().ok_or("Missing operation ID")?)?}),
        "manage" => {
            let revision = operation_id(args.next().ok_or("Missing library revision")?)?;
            let op = operation_id(args.next().ok_or("Missing operation ID")?)?;
            let input = match args.next().ok_or("Missing change JSON")? {
                "-" => read_script(io::stdin().lock())?,
                "--change-file" => {
                    let path = args.next().ok_or("Missing change file path")?;
                    read_script(open_command_file(path)?)?
                }
                text => text.to_owned(),
            };
            let change: super::agent_protocol::LibraryChange =
                serde_json::from_str(&input).map_err(|_| "Invalid library change JSON")?;
            change.validate()?;
            json!({"action":"manage_library","library_revision":revision,"operation_id":op,"change":change})
        }
        "execute" => {
            let id = args
                .next()
                .ok_or("Missing session ID")?
                .parse::<u32>()
                .map_err(|_| "Invalid numeric session ID")?;
            let op = operation_id(args.next().ok_or("Missing operation ID")?)?;
            let command = match args.next().ok_or("Missing command")? {
                "-" => read_script(io::stdin().lock())?,
                "--command-file" => {
                    let path = args.next().ok_or("Missing command file path")?;
                    read_script(open_command_file(path)?)?
                }
                command => {
                    validate_command(command)?;
                    command.to_owned()
                }
            };
            json!({"action":"execute","session_id":id,"operation_id":op,"command":command})
        }
        "upload" => {
            let id = args
                .next()
                .ok_or("Missing session ID")?
                .parse::<u32>()
                .map_err(|_| "Invalid numeric session ID")?;
            let op = operation_id(args.next().ok_or("Missing operation ID")?)?;
            let local = args.next().ok_or("Missing local file or folder path")?;
            let remote = args.next().ok_or("Missing remote destination path")?;
            super::agent_upload::validate_paths(local, remote)?;
            json!({"action":"upload","session_id":id,"operation_id":op,"local":local,"remote":remote})
        }
        "history" => json!({"action":"history"}),
        "poll" | "cancel" => {
            json!({"action":action,"operation_id":operation_id(args.next().ok_or("Missing operation ID")?)?})
        }
        _ => return Err("Unknown subcommand; use --help"),
    };
    while let Some(flag) = args.next() {
        let (key, min, max) = match (action, flag) {
            ("open" | "manage" | "execute" | "upload" | "poll", "--wait-ms") => {
                ("wait_ms", 0, MAX_WAIT_MS)
            }
            ("execute" | "upload", "--timeout-secs") => ("timeout_secs", 1, MAX_COMMAND_SECS),
            ("poll", "--since") => ("since", 0, u64::MAX),
            ("history", "--limit") => ("limit", 1, MAX_JOBS as u64),
            _ => return Err("Unexpected argument; use --help"),
        };
        if request.get(key).is_some() {
            return Err("Duplicate option");
        }
        let value = args
            .next()
            .ok_or("Missing option value")?
            .parse::<u64>()
            .map_err(|_| "Option must be an integer")?;
        if !(min..=max).contains(&value) {
            return Err("Option outside allowed range");
        }
        request[key] = json!(value);
    }
    Ok(request)
}

async fn call(name: &str, expected: &Path, body: &Value) -> io::Result<Value> {
    let bytes = serde_json::to_vec(body)?;
    let wait = Duration::from_millis(
        body.get("wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WAIT_MS)
            .max(3000),
    ) + Duration::from_secs(5);
    let bytes = agent_ipc::request(name, expected, &bytes, wait).await?;
    let result: Value = serde_json::from_slice(&bytes)?;
    if result.get("ok").and_then(Value::as_bool).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid RTerm response",
        ));
    }
    Ok(result)
}

fn error(message: &str) -> Value {
    json!({"ok":false,"error":message})
}

fn print_json(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()
}

pub fn run() -> io::Result<i32> {
    let args: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<_, _>>()
    {
        Ok(args) => args,
        Err(_) => {
            print_json(&error("Arguments must be valid Unicode"))?;
            return Ok(2);
        }
    };
    if args.is_empty() || args == ["--help"] || args == ["-h"] || args == ["help"] {
        println!(
            "rterm-agent {} (protocol {})\n{HELP}",
            env!("CARGO_PKG_VERSION"),
            PROTOCOL_VERSION
        );
        return Ok(0);
    }
    if args == ["--version"] {
        println!(
            "rterm-agent {} (protocol {})",
            env!("CARGO_PKG_VERSION"),
            PROTOCOL_VERSION
        );
        return Ok(0);
    }
    let body = match parse(&args) {
        Ok(body) => body,
        Err(message) => {
            print_json(&error(message))?;
            return Ok(2);
        }
    };
    let result = (|| -> io::Result<Value> {
        let expected = std::fs::canonicalize(std::env::current_exe()?)?
            .with_file_name(format!("rterm{}", std::env::consts::EXE_SUFFIX));
        let name = agent_ipc::pipe_name()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(call(&name, &expected, &body))
    })();
    match result {
        Ok(value) => {
            let code = if value["ok"] == true { 0 } else { 1 };
            print_json(&value)?;
            Ok(code)
        }
        Err(_) => {
            let mut value = error(
                "Local RTerm interface unavailable or disconnected. Check that the matching RTerm executable is running and AI access is explicitly enabled. If execution may have been accepted, poll its original operation ID; never automatically repeat it.",
            );
            if let Some(id) = body.get("operation_id") {
                value["operation_id"] = id.clone();
            }
            print_json(&value)?;
            Ok(3)
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/bin/rterm-agent.rs"]
mod tests;
