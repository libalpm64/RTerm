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
