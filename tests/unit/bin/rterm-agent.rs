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
        assert!(request(&["upload", "42", "upload-1", "code.py", "/home/test/code.py"]).is_err());
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
        assert!(request(&["upload", "42", "upload-1", "../code.py", "/srv/code.py"]).is_err());
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

#[cfg(unix)]
#[test]
fn command_file_rejects_fifos_and_symlinks_before_reading() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("command.fifo");
    let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(open_command_file(fifo.to_str().unwrap()).is_err());

    let target = dir.path().join("command.sh");
    let link = dir.path().join("command-link.sh");
    std::fs::write(&target, b"printf safe\n").unwrap();
    symlink(&target, &link).unwrap();
    assert!(open_command_file(link.to_str().unwrap()).is_err());
    assert_eq!(
        read_script(open_command_file(target.to_str().unwrap()).unwrap()).unwrap(),
        "printf safe\n"
    );
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

#[test]
fn manage_subcommand_validates_and_serializes_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("change.json");
    std::fs::write(
        &path,
        r#"{"kind":"set_port","profile_id":"p-1","port":2222}"#,
    )
    .unwrap();
    assert_eq!(
        request(&[
            "manage",
            "rev",
            "op",
            "--change-file",
            path.to_str().unwrap()
        ])
        .unwrap()["change"]["port"],
        2222
    );

    assert_eq!(
        request(&[
            "manage",
            "rev-1",
            "op-1",
            r#"{"kind":"move_session","profile_id":"p-1","folder":null}"#,
            "--wait-ms",
            "0"
        ])
        .unwrap(),
        json!({"action":"manage_library","library_revision":"rev-1","operation_id":"op-1",
            "change":{"kind":"move_session","profile_id":"p-1","folder":null},"wait_ms":0})
    );
    assert!(
        request(&[
            "manage",
            "rev",
            "op",
            r#"{"kind":"set_port","profile_id":"p","port":0}"#
        ])
        .is_err()
    );
    assert!(
        request(&[
            "manage",
            "rev",
            "op",
            r#"{"kind":"move_session","profile_id":"p"}"#
        ])
        .is_err()
    );
    assert!(
        request(&[
            "manage",
            "rev",
            "op",
            r#"{"kind":"create_folder","name":"A"}"#,
            "--timeout-secs",
            "30"
        ])
        .is_err()
    );
}
