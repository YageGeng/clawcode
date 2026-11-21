//! Image attachments: format and size limits, and the prompt blocks they become.

use agent_client_protocol::v2::schema as wire;
use base64::Engine as _;
use desktop::ui::{AttachmentError, AttachmentSet};

/// A 1×1 transparent PNG.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// The pixel PNG's bytes.
fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PIXEL_PNG)
        .expect("png")
}

/// A PNG-signed buffer of `size` bytes; only the signature decides the format.
fn png_of(size: usize) -> Vec<u8> {
    let mut bytes = png();
    bytes.resize(size, 0);
    bytes
}

#[test]
fn png_is_accepted_and_bmp_rejected() {
    let mut set = AttachmentSet::default();

    set.add("pixel.png".to_string(), png()).expect("png");
    let bmp = set.add("pixel.bmp".to_string(), b"BM\0\0\0\0\0\0\0\0".to_vec());

    assert_eq!(bmp, Err(AttachmentError::UnsupportedFormat));
    assert_eq!(set.len(), 1);
    assert_eq!(set.items()[0].mime, "image/png");
}

#[test]
fn sixth_image_is_rejected() {
    let mut set = AttachmentSet::default();
    for n in 0..5 {
        set.add(format!("{n}.png"), png())
            .expect("within the limit");
    }

    assert_eq!(
        set.add("6.png".to_string(), png()),
        Err(AttachmentError::TooMany)
    );
}

#[test]
fn oversize_and_total_limits() {
    const MIB: usize = 1024 * 1024;
    let mut set = AttachmentSet::default();

    assert_eq!(
        set.add("big.png".to_string(), png_of(10 * MIB + 1)),
        Err(AttachmentError::TooLarge)
    );
    set.add("a.png".to_string(), png_of(7 * MIB))
        .expect("first");
    set.add("b.png".to_string(), png_of(7 * MIB))
        .expect("second");
    assert_eq!(
        set.add("c.png".to_string(), png_of(7 * MIB)),
        Err(AttachmentError::TotalTooLarge)
    );
}

#[test]
fn blocks_are_standard_base64_images() {
    let mut set = AttachmentSet::default();
    set.add("pixel.png".to_string(), png()).expect("png");
    set.remove(5);

    let blocks = set.into_blocks();

    assert_eq!(blocks.len(), 1);
    let wire::ContentBlock::Image(image) = &blocks[0] else {
        panic!("image block");
    };
    assert_eq!(image.data, PIXEL_PNG);
    assert_eq!(image.mime_type.0.as_ref(), "image/png");
}

#[test]
fn prebuilt_image_matches_the_bytes() {
    let mut set = AttachmentSet::default();
    set.add("pixel.png".to_string(), png()).expect("png");

    let item = &set.items()[0];

    assert_eq!(item.image.bytes(), png().as_slice());
    assert_eq!(item.image.format(), gpui_kit::ImageFormat::Png);
}

#[test]
fn file_length_is_checked_before_reading() {
    const MIB: u64 = 1024 * 1024;
    let mut set = AttachmentSet::default();

    assert_eq!(set.admits(10 * MIB + 1), Err(AttachmentError::TooLarge));
    assert_eq!(set.admits(MIB), Ok(()));
    set.add("a.png".to_string(), png_of(15 * 1024 * 1024))
        .expect_err("too large");
    set.add("a.png".to_string(), png_of(9 * 1024 * 1024))
        .expect("first");
    set.add("b.png".to_string(), png_of(9 * 1024 * 1024))
        .expect("second");
    assert_eq!(set.admits(3 * MIB), Err(AttachmentError::TotalTooLarge));
}
