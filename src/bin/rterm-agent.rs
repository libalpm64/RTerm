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

mod cli {
    use super::{
        agent_ipc,
        agent_protocol::{
            DEFAULT_WAIT_MS, MAX_COMMAND_SECS, MAX_JOBS, MAX_SCRIPT, MAX_WAIT_MS, PROTOCOL_VERSION,
            valid_id, validate_command,
        },
    };
    use serde_json::{Value, json};
    use std::{
        io::{self, Read, Write},
        path::Path,
        time::Duration,
    };

    const HELP: &str = "RTerm local agent utility\n\
Usage:\n\
  rterm-agent sessions\n\
  rterm-agent open <profile-id> <operation-id> [--wait-ms N]\n\
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

    fn parse(args: &[String]) -> Result<Value, &'static str> {
        let mut args = args.iter().map(String::as_str);
        let action = args.next().ok_or("Missing subcommand; use --help")?;
        let mut request = match action {
            "sessions" => json!({"action":"list_sessions"}),
            "open" => json!({"action":"open_session",
                "profile_id":operation_id(args.next().ok_or("Missing profile ID")?)?,
                "operation_id":operation_id(args.next().ok_or("Missing operation ID")?)?}),
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
                        let file =
                            std::fs::File::open(path).map_err(|_| "Cannot open command file")?;
                        if !file
                            .metadata()
                            .map_err(|_| "Cannot inspect command file")?
                            .is_file()
                        {
                            return Err("Command file must be a regular file");
                        }
                        read_script(file)?
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
                ("open" | "execute" | "upload" | "poll", "--wait-ms") => {
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
    mod tests {
        use super::*;
        #[test]
        fn script_input_preserves_quotes_and_multiline_content() {
            let script = "\u{feff}echo \"$(date)\"\r\nprintf '%s' ':80 (LISTEN)'\r\n";
            let expected = "echo \"$(date)\"\nprintf '%s' ':80 (LISTEN)'\n";
            assert_eq!(read_script(script.as_bytes()).unwrap(), expected);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("diagnostic script.sh");
            std::fs::write(&path, script).unwrap();
            let body = request(&[
                "execute",
                "42",
                "script-test",
                "--command-file",
                path.to_str().unwrap(),
                "--wait-ms",
                "0",
            ])
            .unwrap();
            assert_eq!(body["command"], expected);
            assert!(read_script(&vec![b'x'; MAX_SCRIPT + 1][..]).is_err());
            assert!(read_script(&b"bad\0script"[..]).is_err());
            assert_eq!(
                request(&["poll", "script-test", "--since", "128"]).unwrap()["since"],
                128
            );
            assert_eq!(request(&["history", "--limit", "20"]).unwrap()["limit"], 20);
        }
        fn request(args: &[&str]) -> Result<Value, &'static str> {
            parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        }

        #[test]
        fn accepts_only_documented_subcommands() {
            assert_eq!(
                request(&["sessions"]).unwrap(),
                json!({"action":"list_sessions"})
            );
            assert_eq!(
                request(&["open", "profile-1", "op-1", "--wait-ms", "0"]).unwrap(),
                json!({"action":"open_session","profile_id":"profile-1","operation_id":"op-1","wait_ms":0})
            );
            assert!(request(&["open", "profile-1"]).is_err());
            assert!(request(&["open", "../profile", "op-1"]).is_err());
            assert!(request(&["open", "profile-1", "op-1", "--timeout-secs", "30"]).is_err());
            #[cfg(windows)]
            {
                assert_eq!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        r"C:\Projects\code.py",
                        "/home/test/code.py",
                        "--wait-ms",
                        "0"
                    ])
                    .unwrap(),
                    json!({"action":"upload","session_id":42,"operation_id":"upload-1","local":r"C:\Projects\code.py","remote":"/home/test/code.py","wait_ms":0})
                );
                assert!(
                    request(&["upload", "42", "upload-1", "code.py", "/home/test/code.py"])
                        .is_err()
                );
                assert!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        r"C:\Projects\code.py",
                        "relative.py"
                    ])
                    .is_err()
                );
                assert!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        r"C:\Projects\code.py:stream",
                        "/home/test/code.py"
                    ])
                    .is_err()
                );
                assert!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        r"C:\Projects\NUL.txt",
                        "/home/test/code.py"
                    ])
                    .is_err()
                );
            }
            #[cfg(unix)]
            {
                assert_eq!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        "/home/test/code.py",
                        "/srv/code.py",
                        "--wait-ms",
                        "0"
                    ])
                    .unwrap()["local"],
                    "/home/test/code.py"
                );
                assert!(
                    request(&["upload", "42", "upload-1", "../code.py", "/srv/code.py"]).is_err()
                );
                assert!(
                    request(&[
                        "upload",
                        "42",
                        "upload-1",
                        "/home/test/../code.py",
                        "/srv/code.py"
                    ])
                    .is_err()
                );
            }
            assert_eq!(
                request(&[
                    "execute",
                    "42",
                    "test-1",
                    "echo \"quoted\"; pwd",
                    "--wait-ms",
                    "0",
                    "--timeout-secs",
                    "600"
                ])
                .unwrap(),
                json!({"action":"execute","session_id":42,"operation_id":"test-1","command":"echo \"quoted\"; pwd","wait_ms":0,"timeout_secs":600})
            );
            assert_eq!(
                request(&["poll", "test-1", "--wait-ms", "5000"]).unwrap(),
                json!({"action":"poll","operation_id":"test-1","wait_ms":5000})
            );
            assert_eq!(
                request(&["cancel", "test-1"]).unwrap(),
                json!({"action":"cancel","operation_id":"test-1"})
            );
            assert!(request(&["sessions", "extra"]).is_err());
            assert!(request(&["execute", "4294967296", "test", "pwd"]).is_err());
            assert!(request(&["execute", "1", "test"]).is_err());
            assert!(request(&["execute", "1", "test", " "]).is_err());
            assert!(request(&["execute", "1", "bad/id", "pwd"]).is_err());
            assert!(request(&["execute", "1", "test", "pwd", "--timeout-secs", "0"]).is_err());
            assert!(request(&["poll", "test", "--wait-ms", "30001"]).is_err());
            assert!(request(&["poll", "test", "--wait-ms", "1", "--wait-ms", "2"]).is_err());
            assert!(request(&["cancel", "test", "--wait-ms", "1"]).is_err());
            assert!(request(&["execute", "1", "test", &"x".repeat(MAX_SCRIPT + 1)]).is_err());
        }

        #[cfg(windows)]
        #[test]
        fn cli_request_round_trips_on_isolated_pipe() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let name = format!(
                    "{}.cli-test.{}",
                    agent_ipc::pipe_name().unwrap(),
                    std::process::id()
                );
                let mut listener = agent_ipc::listen(&name, true).unwrap();
                let expected = std::env::current_exe().unwrap();
                let body = request(&["sessions"]).unwrap();
                let expected_response =
                    json!({"ok":true,"sessions":[{"session_id":42,"name":"Fixture only"}]});
                let response = expected_response.clone();
                let server = tokio::spawn(async move {
                    listener.connect().await.unwrap();
                    let bytes = agent_ipc::read_frame(&mut listener, agent_ipc::MAX_REQUEST)
                        .await
                        .unwrap();
                    assert_eq!(
                        serde_json::from_slice::<Value>(&bytes).unwrap(),
                        json!({"action":"list_sessions"})
                    );
                    agent_ipc::write_frame(
                        &mut listener,
                        &serde_json::to_vec(&response).unwrap(),
                        agent_ipc::MAX_RESPONSE,
                    )
                    .await
                    .unwrap();
                });
                assert_eq!(
                    call(&name, &expected, &body).await.unwrap(),
                    expected_response
                );
                server.await.unwrap();
            });
        }
    }
}

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
