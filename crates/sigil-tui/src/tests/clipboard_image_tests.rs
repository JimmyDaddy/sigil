use image::GenericImageView;

use super::encode_clipboard_rgba;

#[test]
fn clipboard_rgba_is_encoded_as_valid_png() -> anyhow::Result<()> {
    let encoded = encode_clipboard_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255])?;
    let decoded = image::load_from_memory(&encoded)?;

    assert_eq!(decoded.dimensions(), (2, 1));
    assert_eq!(
        decoded.to_rgba8().as_raw(),
        &[255, 0, 0, 255, 0, 255, 0, 255]
    );
    Ok(())
}

#[test]
fn clipboard_rgba_rejects_mismatched_buffers() {
    let error = encode_clipboard_rgba(2, 1, &[0; 4]).expect_err("short RGBA buffer must fail");
    assert!(error.to_string().contains("tightly packed RGBA"));
}
