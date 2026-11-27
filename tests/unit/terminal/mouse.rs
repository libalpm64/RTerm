use super::*;

#[test]
fn sgr_and_legacy_reports_preserve_buttons_coordinates_and_modifiers() {
    let none = Modifiers::NONE;
    let sgr = TermMode::SGR_MOUSE;
    assert_eq!(report(sgr, 0, 5, 2, false, none).unwrap(), b"\x1b[<0;6;3M");
    assert_eq!(report(sgr, 2, 5, 2, true, none).unwrap(), b"\x1b[<2;6;3m");
    assert_eq!(
        report(
            sgr,
            32,
            8,
            9,
            false,
            Modifiers {
                ctrl: true,
                alt: true,
                ..none
            }
        )
        .unwrap(),
        b"\x1b[<56;9;10M"
    );
    assert_eq!(
        report(sgr, 64, 3, 4, false, none).unwrap(),
        b"\x1b[<64;4;5M"
    );
    assert_eq!(
        report(TermMode::NONE, 0, 5, 2, false, none).unwrap(),
        b"\x1b[M &\x23"
    );
    assert_eq!(
        report(TermMode::NONE, 2, 5, 2, true, none).unwrap(),
        b"\x1b[M#&\x23"
    );
    assert!(report(TermMode::NONE, 0, 223, 1, false, none).is_none());
    assert_eq!(
        report(TermMode::UTF8_MOUSE, 0, 223, 1, false, none).unwrap(),
        b"\x1b[M \xc4\x80\x22"
    );
}
