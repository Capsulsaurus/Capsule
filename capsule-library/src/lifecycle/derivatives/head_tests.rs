//! A still passed to [`Workspace::prepare_still`] by its head alone.

use tempfile::TempDir;

use super::super::DerivativeStatus;
use super::tests::{jpeg, workspace};
use super::*;

/// A still the importer declined to hold — only its head was passed, for a file larger than
/// that head — is not decoded, even when the head alone is a whole decodable JPEG: it reports
/// `DecodeFailed` with the header-derived format and EXIF dimensions, and nothing is
/// generated. Passed whole, the same bytes decode.
#[test]
fn a_still_passed_by_its_head_alone_is_not_decoded() {
    let lib = TempDir::new().unwrap();
    let (ws, album) = workspace(lib.path());
    let keys = ws.album(&album).unwrap();
    let amk = Amk::from_bytes(keys.amks[&keys.current_epoch]);
    let asset_id = Uuid::from_u128(0xB16);
    let bytes = jpeg(32, 24, None);
    let (original, _, _) =
        capsule_core::crypto::encryption::encrypt_asset_rekey(&amk, &asset_id, &bytes, None)
            .unwrap();
    let exif = ExifExtract {
        width: Some(32),
        height: Some(24),
        ..ExifExtract::default()
    };
    let prepare = |size: u64| {
        ws.prepare_still(
            &StillSource {
                plaintext: &bytes,
                size,
                ext: "jpg",
                src: Path::new("big.jpg"),
                exif: &exif,
            },
            asset_id,
            album,
            0,
            &amk,
            &original,
        )
        .unwrap()
    };

    let declined = prepare(bytes.len() as u64 + 1);
    assert_eq!(declined.status, DerivativeStatus::DecodeFailed);
    assert_eq!(declined.format, Some(StillFormat::Jpeg));
    assert_eq!(
        declined.dimensions,
        Some(Dimensions {
            width: 32,
            height: 24
        })
    );
    assert!(declined.lqip.is_none());
    assert!(declined.derivatives.is_empty());

    let whole = prepare(bytes.len() as u64);
    assert_eq!(whole.status, DerivativeStatus::Decoded);
    assert!(whole.lqip.is_some());
}
