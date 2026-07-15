use std::io::Cursor;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageFormat, RgbaImage};
use sigil_kernel::{MAX_IMAGE_ATTACHMENT_DIMENSION, MAX_IMAGE_ATTACHMENT_PIXELS};

#[cfg(not(test))]
pub(crate) fn read_clipboard_image_png() -> Result<Option<Vec<u8>>> {
    let mut clipboard = arboard::Clipboard::new().context("failed to open the system clipboard")?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(error) => return Err(error).context("failed to read an image from the clipboard"),
    };
    encode_clipboard_rgba(image.width, image.height, image.bytes.as_ref()).map(Some)
}

pub(crate) fn encode_clipboard_rgba(
    width: usize,
    height: usize,
    rgba_bytes: &[u8],
) -> Result<Vec<u8>> {
    let width_u32 = u32::try_from(width).context("clipboard image width is too large")?;
    let height_u32 = u32::try_from(height).context("clipboard image height is too large")?;
    if width_u32 == 0
        || height_u32 == 0
        || width_u32 > MAX_IMAGE_ATTACHMENT_DIMENSION
        || height_u32 > MAX_IMAGE_ATTACHMENT_DIMENSION
    {
        bail!("clipboard image dimensions are outside the V1 limits");
    }
    let pixels = width
        .checked_mul(height)
        .context("clipboard image pixel count overflowed")?;
    if pixels as u64 > MAX_IMAGE_ATTACHMENT_PIXELS {
        bail!("clipboard image pixel count exceeds the V1 limit");
    }
    let expected_bytes = pixels
        .checked_mul(4)
        .context("clipboard image byte count overflowed")?;
    if rgba_bytes.len() != expected_bytes {
        bail!("clipboard image is not tightly packed RGBA data");
    }
    let image = RgbaImage::from_raw(width_u32, height_u32, rgba_bytes.to_vec())
        .context("clipboard RGBA buffer does not match its dimensions")?;
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .context("failed to encode clipboard image as PNG")?;
    Ok(encoded.into_inner())
}

#[cfg(test)]
#[path = "tests/clipboard_image_tests.rs"]
mod tests;
