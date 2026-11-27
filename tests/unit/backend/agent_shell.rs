use super::*;

#[test]
fn frame_is_a_single_terminal_line_and_preserves_quotes() {
    let line = frame("cd '/tmp/a b'; printf \"%s\" 'hello'\n", "fixture").unwrap();
    let line = String::from_utf8(line).unwrap();
    assert!(line.ends_with('\r'));
    assert!(!line.contains('\n'));
    assert!(!line.contains('\x1b'));
    assert!(line.contains("eval"));
    assert!(line.contains("\\0012"));
    assert!(frame("bad\x03", "fixture").is_err());
    assert!(frame(&"x".repeat(3500), "fixture").is_err());
}
