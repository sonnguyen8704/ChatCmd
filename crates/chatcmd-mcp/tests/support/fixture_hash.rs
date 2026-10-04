/// Text fixtures have identical content under Git's LF and CRLF checkouts.
/// Preserve all bytes except carriage returns immediately before a newline.
pub(super) fn normalize_line_endings(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            (!(*byte == b'\r' && bytes.get(index + 1) == Some(&b'\n'))).then_some(*byte)
        })
        .collect()
}

#[test]
fn checkout_line_endings_do_not_change_fixture_content() {
    assert_eq!(normalize_line_endings(b"one\r\ntwo\r\n"), b"one\ntwo\n");
    assert_eq!(normalize_line_endings(b"one\ntwo\n"), b"one\ntwo\n");
    // A real content change and a standalone CR must remain detectable.
    assert_ne!(normalize_line_endings(b"one\r\nchanged\r\n"), b"one\ntwo\n");
    assert_eq!(normalize_line_endings(b"one\rtwo"), b"one\rtwo");
}
