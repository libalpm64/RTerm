use super::*;

#[test]
fn ranges_into_reuses_storage_and_preserves_match_order() {
    let highlighter = Highlighter::new(true, "error success");
    let mut ranges = Vec::with_capacity(32);
    highlighter.ranges_into("sudo rm failed success", &mut ranges);
    let first = ranges.clone();
    let capacity = ranges.capacity();
    let pointer = ranges.as_ptr();

    highlighter.ranges_into("sudo rm failed success", &mut ranges);

    assert_eq!(ranges, first);
    assert_eq!(ranges.capacity(), capacity);
    assert_eq!(ranges.as_ptr(), pointer);
    assert!(ranges.len() >= 4);
}

#[test]
fn keyword_color_classification_is_ascii_case_insensitive() {
    assert!(contains_ascii_case_insensitive("Pre-ERROR-Code", "error"));
    assert!(contains_ascii_case_insensitive("SUCCESS", "success"));
    assert!(!contains_ascii_case_insensitive("fatal", "error"));
}
