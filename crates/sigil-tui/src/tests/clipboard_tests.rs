use std::cell::Cell;

use anyhow::bail;

use super::{
    ClipboardCopyOutcome, base64_encode, copy_text_with, osc52_clipboard_sequence,
    wrap_osc52_for_multiplexer,
};

#[test]
fn osc52_clipboard_sequence_encodes_text() {
    assert_eq!(base64_encode(b"h"), "aA==");
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(osc52_clipboard_sequence("hi"), "\x1b]52;c;aGk=\x07");
}

#[test]
fn osc52_clipboard_sequence_uses_multiplexer_passthrough() {
    let sequence = osc52_clipboard_sequence("hi");

    assert_eq!(
        wrap_osc52_for_multiplexer(sequence.clone(), true, false),
        "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
    );
    assert_eq!(
        wrap_osc52_for_multiplexer(sequence.clone(), false, true),
        "\x1bP\x1b]52;c;aGk=\x07\x1b\\"
    );
    assert_eq!(
        wrap_osc52_for_multiplexer(sequence.clone(), false, false),
        sequence
    );
}

#[test]
fn copy_text_attempts_system_clipboard_after_osc52_succeeds() {
    let osc52_called = Cell::new(false);
    let system_called = Cell::new(false);

    let outcome = copy_text_with(
        "selected",
        true,
        |_| {
            osc52_called.set(true);
            Ok(())
        },
        |_| {
            system_called.set(true);
            Ok(())
        },
    );

    assert_eq!(outcome, ClipboardCopyOutcome::Copied);
    assert!(osc52_called.get());
    assert!(system_called.get());
}

#[test]
fn copy_text_uses_system_clipboard_when_osc52_is_disabled() {
    let osc52_called = Cell::new(false);

    let outcome = copy_text_with(
        "selected",
        false,
        |_| {
            osc52_called.set(true);
            bail!("OSC52 should stay disabled")
        },
        |_| Ok(()),
    );

    assert_eq!(outcome, ClipboardCopyOutcome::Copied);
    assert!(!osc52_called.get());
}

#[test]
fn copy_text_succeeds_when_only_one_backend_is_available() {
    let outcome = copy_text_with("selected", true, |_| bail!("OSC52 unavailable"), |_| Ok(()));

    assert_eq!(outcome, ClipboardCopyOutcome::Copied);
}

#[test]
fn copy_text_reports_when_both_backends_are_unavailable() {
    let outcome = copy_text_with(
        "selected",
        true,
        |_| bail!("OSC52 unavailable"),
        |_| bail!("system clipboard unavailable"),
    );

    assert_eq!(
        outcome,
        ClipboardCopyOutcome::Unavailable(
            "system clipboard unavailable; OSC52 write failed".to_owned()
        )
    );
}

#[test]
fn copy_text_reports_disabled_osc52_when_system_clipboard_is_unavailable() {
    let outcome = copy_text_with(
        "selected",
        false,
        |_| Ok(()),
        |_| bail!("system clipboard unavailable"),
    );

    assert_eq!(
        outcome,
        ClipboardCopyOutcome::Unavailable(
            "system clipboard unavailable; OSC52 disabled".to_owned()
        )
    );
}
