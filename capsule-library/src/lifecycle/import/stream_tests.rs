//! The streamed original: the head read, the tee, the staged library copy, and the size bound
//! on the EXIF scan.

use std::fs;
use std::io::Read;

use tempfile::TempDir;

use super::*;

/// The head read identifies a file without consuming it: the stream that follows starts at
/// byte 0, and a file shorter than the sniff window is read whole.
#[test]
fn read_head_rewinds_and_stops_at_the_sniff_window() {
    let dir = TempDir::new().unwrap();
    let long = dir.path().join("long.bin");
    let bytes: Vec<u8> = (0..200u8).collect();
    fs::write(&long, &bytes).unwrap();
    let mut file = fs::File::open(&long).unwrap();
    assert_eq!(read_head(&mut file).unwrap(), &bytes[..SNIFF_LEN as usize]);
    let mut rest = Vec::new();
    file.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, bytes, "the head read rewinds");

    let short = dir.path().join("short.bin");
    fs::write(&short, b"tiny").unwrap();
    assert_eq!(
        read_head(&mut fs::File::open(&short).unwrap()).unwrap(),
        b"tiny"
    );
}

/// The tee hands the encryptor every byte unchanged, hashes exactly those bytes, and copies
/// exactly those bytes.
#[test]
fn tee_hashes_and_copies_what_it_passes() {
    let dir = TempDir::new().unwrap();
    let copy_path = dir.path().join("copy");
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
    let mut tee = Tee {
        inner: bytes.as_slice(),
        hasher: Sha256Hasher::new(),
        copy: Some(fs::File::create(&copy_path).unwrap()),
    };
    let mut passed = Vec::new();
    tee.read_to_end(&mut passed).unwrap();
    let Tee { hasher, copy, .. } = tee;
    drop(copy);
    assert_eq!(passed, bytes);
    assert_eq!(hasher.finalize(), hash::hash_bytes(&bytes));
    assert_eq!(fs::read(&copy_path).unwrap(), bytes);
}

/// A staged original that is never committed — the import failed after the copy began —
/// takes its `.tmp` with it; a committed one lands at its media path.
#[test]
fn a_staged_original_is_removed_unless_committed() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("asset.mp4");
    let tmp = tmp_path(&target);

    fs::write(&tmp, b"plaintext").unwrap();
    drop(StagedOriginal(Some(Staged {
        tmp: tmp.clone(),
        target: target.clone(),
    })));
    assert!(
        !tmp.exists(),
        "an abandoned copy leaves no plaintext behind"
    );
    assert!(!target.exists());

    fs::write(&tmp, b"plaintext").unwrap();
    StagedOriginal(Some(Staged {
        tmp: tmp.clone(),
        target: target.clone(),
    }))
    .commit()
    .unwrap();
    assert!(!tmp.exists());
    assert_eq!(fs::read(&target).unwrap(), b"plaintext");

    // In place: nothing staged, nothing to commit.
    StagedOriginal(None).commit().unwrap();
}

/// A JPEG holding only an EXIF APP1 segment whose IFD0 carries `Make = "Cap"`, inline.
fn jpeg_with_make() -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&0x002Au16.to_be_bytes());
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 right after the header
    tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
    tiff.extend_from_slice(&0x010Fu16.to_be_bytes()); // Make
    tiff.extend_from_slice(&2u16.to_be_bytes()); // ASCII
    tiff.extend_from_slice(&4u32.to_be_bytes()); // "Cap\0" fits inline
    tiff.extend_from_slice(b"Cap\0");
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD

    let mut app1 = Vec::from(*b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
    let len = u16::try_from(app1.len() + 2).unwrap();
    jpeg.extend_from_slice(&len.to_be_bytes());
    jpeg.extend_from_slice(&app1);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}

/// The EXIF scan is bounded by the file's size for every container the reader parses: a file
/// at the cap is scanned, and one a byte over it is not read at all, whatever its header
/// claims. The over-cap files are sparse and never read, so they cost no disk and no time.
#[test]
fn the_exif_scan_is_bounded_by_size_whatever_the_container() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("photo.jpg");
    fs::write(&path, jpeg_with_make()).unwrap();
    // `size` is the caller's stat of the file; the decision reads nothing else.
    assert_eq!(
        scan_exif(&path, MAX_HELD_STILL_BYTES).make.as_deref(),
        Some("Cap"),
        "a file at the cap is scanned"
    );

    let mut heif = vec![0, 0, 0, 0];
    heif.extend_from_slice(b"ftypheic");
    let heads: [(&str, Vec<u8>); 5] = [
        ("over.jpg", jpeg_with_make()),
        ("over.tif", b"MM\x00\x2A\x00\x00\x00\x08".to_vec()),
        ("over.heic", heif),
        ("over.png", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("over.webp", b"RIFF\0\0\0\0WEBP".to_vec()),
    ];
    for (name, head) in heads {
        let path = dir.path().join(name);
        fs::write(&path, head).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_HELD_STILL_BYTES + 1)
            .unwrap();
        assert_eq!(
            scan_exif(&path, MAX_HELD_STILL_BYTES + 1),
            ExifExtract::default(),
            "{name}: a file over the cap is not scanned"
        );
    }
}
