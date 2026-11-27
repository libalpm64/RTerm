use super::*;

#[test]
fn launcher_icon_remains_available() {
    let launcher = window();
    assert_eq!((launcher.width, launcher.height), (64, 64));
    assert_eq!(launcher.rgba.len(), 64 * 64 * 4);
}
