use super::*;

#[tokio::test]
async fn frame_round_trip_and_invalid_lengths() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    write_frame(&mut writer, b"test", 4).await.unwrap();
    assert_eq!(read_frame(&mut reader, 4).await.unwrap(), b"test");
    for bytes in [&b""[..], &b"12345"[..]] {
        assert_eq!(
            write_frame(&mut writer, bytes, 4).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    for length in [0, 5, u32::MAX] {
        writer.write_u32_le(length).await.unwrap();
        assert_eq!(
            read_frame(&mut reader, 4).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    writer.write_u32_le(4).await.unwrap();
    writer.write_all(b"x").await.unwrap();
    writer.shutdown().await.unwrap();
    assert_eq!(
        read_frame(&mut reader, 4).await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn protocol_limits_are_inclusive() {
    assert!(valid_id(&"a".repeat(64)));
    assert!(!valid_id(&"a".repeat(65)));
    assert!(!valid_id("a/b"));
    assert!(!valid_id(""));
    assert!(validate_command(&"x".repeat(MAX_SCRIPT)).is_ok());
    assert!(validate_command(&"x".repeat(MAX_SCRIPT + 1)).is_err());
    assert!(validate_command(" \t\n").is_err());
    assert!(validate_command("x\0y").is_err());
}

#[test]
fn library_changes_require_explicit_valid_fields() {
    let parse = |json: &str| -> Result<LibraryChange, String> {
        let change: LibraryChange = serde_json::from_str(json).map_err(|e| e.to_string())?;
        change.validate().map_err(str::to_owned)?;
        Ok(change)
    };
    for input in [
        r#"{"kind":"create_folder","name":"Production"}"#,
        r#"{"kind":"rename_folder","folder":"Production","name":"New"}"#,
        r#"{"kind":"remove_folder","folder":"Production"}"#,
        r#"{"kind":"move_session","profile_id":"abc","folder":null}"#,
        r#"{"kind":"move_session","profile_id":"abc","folder":"New"}"#,
        r#"{"kind":"rename_session","profile_id":"abc","name":"Server"}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":1}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":65535}"#,
    ] {
        assert!(parse(input).is_ok(), "{input}");
    }
    for input in [
        r#"{"kind":"create_folder","name":" "}"#,
        r#"{"kind":"create_folder","name":"a\u202eb"}"#,
        r#"{"kind":"create_folder","name":"a","password":"secret"}"#,
        r#"{"kind":"move_session","profile_id":"abc"}"#,
        r#"{"kind":"move_session","profile_id":"../abc","folder":null}"#,
        r#"{"kind":"rename_session","profile_id":"abc","name":" a "}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":0}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":65536}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":"22"}"#,
        r#"{"kind":"set_port","profile_id":"abc","port":22,"host":"other"}"#,
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
    assert!(!valid_library_name(&"a".repeat(129)));
}
