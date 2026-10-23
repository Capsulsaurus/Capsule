use std::sync::Mutex;

use exif::{In, Tag};
use image::{Rgb, RgbImage};

use super::*;

/// Serves canned bytes per URL and records every fetch.
struct FakeFetcher {
    bytes: Vec<(String, Vec<u8>)>,
    calls: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn new(bytes: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            bytes,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Fetcher for FakeFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> Result<()> {
        self.calls.lock().unwrap().push(url.to_owned());
        let (_, bytes) = self
            .bytes
            .iter()
            .find(|(u, _)| u == url)
            .ok_or_else(|| eyre!("404 {url}"))?;
        fs::write(dest, bytes)?;
        Ok(())
    }
}

/// A small gradient JPEG with a given size — distinct content per seed value.
fn jpeg(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, seed])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95)
        .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn manifest_for(photos: &[(&str, &[u8])]) -> Manifest {
    use std::fmt::Write as _;
    let mut text = String::new();
    for (i, (id, bytes)) in photos.iter().enumerate() {
        write!(
            text,
            "[[photo]]\nid = \"{id}\"\nday_offset = {}\ntime = \"12:00\"\nurl = \"https://example.test/{id}.jpg\"\n\
                 sha256 = \"{}\"\nsource_page = \"https://example.test/{id}\"\nauthor = \"A\"\nlicense = \"CC0-1.0\"\n",
            i + 2,
            hex::encode(Sha256::digest(bytes))
        )
        .unwrap();
    }
    Manifest::parse(&text).unwrap()
}

fn fetcher_for(photos: &[(&str, &[u8])]) -> FakeFetcher {
    FakeFetcher::new(
        photos
            .iter()
            .map(|(id, b)| (format!("https://example.test/{id}.jpg"), b.to_vec()))
            .collect(),
    )
}

#[test]
fn cache_fetches_once_then_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = jpeg(8, 8, 1);
    let photos = [("a", bytes.as_slice())];
    let manifest = manifest_for(&photos);
    let fetcher = fetcher_for(&photos);

    let (path, first) = ensure_cached(&manifest.photos[0], tmp.path(), &fetcher).unwrap();
    let (_, second) = ensure_cached(&manifest.photos[0], tmp.path(), &fetcher).unwrap();
    assert_eq!((first, second), (CacheOutcome::Fetched, CacheOutcome::Hit));
    assert_eq!(fetcher.calls(), 1);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn corrupted_cache_entry_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = jpeg(8, 8, 1);
    let photos = [("a", bytes.as_slice())];
    let manifest = manifest_for(&photos);
    let fetcher = fetcher_for(&photos);
    fs::write(
        tmp.path()
            .join(format!("{}.jpg", manifest.photos[0].sha256)),
        b"stale",
    )
    .unwrap();

    let (path, outcome) = ensure_cached(&manifest.photos[0], tmp.path(), &fetcher).unwrap();
    assert_eq!(outcome, CacheOutcome::Fetched);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn bytes_that_fail_the_pin_are_rejected_and_not_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let pinned = jpeg(8, 8, 1);
    let manifest = manifest_for(&[("a", pinned.as_slice())]);
    let served = jpeg(8, 8, 2);
    let fetcher = fetcher_for(&[("a", served.as_slice())]);

    let err = ensure_cached(&manifest.photos[0], tmp.path(), &fetcher).unwrap_err();
    assert!(
        format!("{err:#}").contains("do not match the manifest pin"),
        "{err:#}"
    );
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "nothing left behind"
    );
}

#[test]
fn normalize_rotates_by_the_source_orientation() {
    // A 60×20 landscape JPEG whose EXIF says "rotate 90° clockwise" (Orientation = 6).
    let mut exif = capture_exif::capture_date_block(jiff::civil::date(2020, 1, 1).at(0, 0, 0, 0));
    exif[18..20].copy_from_slice(&6u16.to_be_bytes());
    let img = RgbImage::from_pixel(60, 20, Rgb([200, 100, 50]));
    let mut src = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut src, 95);
    encoder.set_exif_metadata(exif).unwrap();
    encoder
        .write_image(img.as_raw(), 60, 20, image::ExtendedColorType::Rgb8)
        .unwrap();

    let out = normalize(&src, jiff::civil::date(2026, 1, 1).at(9, 0, 0, 0)).unwrap();
    let decoded = image::load_from_memory(&out).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (20, 60),
        "stored upright"
    );
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(&out))
        .unwrap();
    let orientation = exif.get_field(Tag::Orientation, In::PRIMARY).unwrap();
    assert_eq!(
        orientation.value.get_uint(0),
        Some(1),
        "and declared upright"
    );
}

#[test]
fn only_rgb_icc_profiles_are_carried_over() {
    let mut header = vec![0u8; 128];
    header[16..20].copy_from_slice(b"RGB ");
    assert!(is_rgb_profile(&header));
    header[16..20].copy_from_slice(b"GRAY");
    assert!(!is_rgb_profile(&header));
    header[16..20].copy_from_slice(b"CMYK");
    assert!(!is_rgb_profile(&header));
    assert!(!is_rgb_profile(b"short"));
}

#[test]
fn normalize_bounds_the_long_edge_and_keeps_aspect() {
    let out = normalize(
        &jpeg(3000, 1500, 7),
        jiff::civil::date(2026, 1, 1).at(9, 0, 0, 0),
    )
    .unwrap();
    let img = image::load_from_memory(&out).unwrap();
    assert_eq!((img.width(), img.height()), (MAX_EDGE, MAX_EDGE / 2));
}

#[test]
fn normalize_leaves_small_images_at_their_size() {
    let out = normalize(
        &jpeg(640, 480, 7),
        jiff::civil::date(2026, 1, 1).at(9, 0, 0, 0),
    )
    .unwrap();
    let img = image::load_from_memory(&out).unwrap();
    assert_eq!((img.width(), img.height()), (640, 480));
}

#[test]
fn normalize_writes_the_capture_date() {
    let out = normalize(
        &jpeg(64, 64, 7),
        jiff::civil::date(2026, 9, 20).at(17, 40, 0, 0),
    )
    .unwrap();
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(&out))
        .unwrap();
    let field = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY).unwrap();
    assert_eq!(field.display_value().to_string(), "2026-09-20 17:40:00");
}

#[test]
fn normalize_is_byte_deterministic() {
    let src = jpeg(2500, 1200, 3);
    let at = jiff::civil::date(2026, 5, 5).at(5, 5, 5, 0);
    assert_eq!(normalize(&src, at).unwrap(), normalize(&src, at).unwrap());
}

#[test]
fn run_is_idempotent_and_fetches_only_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (jpeg(40, 30, 1), jpeg(30, 40, 2));
    let photos = [("first", a.as_slice()), ("second", b.as_slice())];
    let manifest = manifest_for(&photos);
    let fetcher = fetcher_for(&photos);
    let layout = Layout::under(tmp.path());
    let anchor = jiff::civil::date(2026, 9, 22);

    let first = run(&manifest, &layout, anchor, &fetcher).unwrap();
    let second = run(&manifest, &layout, anchor, &fetcher).unwrap();
    assert_eq!(first, second);
    assert_eq!(fetcher.calls(), 2, "second run served from cache");

    let mut names: Vec<_> = fs::read_dir(&layout.out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["01-first.jpg", "02-second.jpg"]);
    assert!(!layout.out.with_extension("staging").exists());
}

#[test]
fn run_rebuilds_the_directory_without_stale_files() {
    let tmp = tempfile::tempdir().unwrap();
    let a = jpeg(40, 30, 1);
    let photos = [("only", a.as_slice())];
    let layout = Layout::under(tmp.path());
    fs::create_dir_all(&layout.out).unwrap();
    fs::write(layout.out.join("99-removed.jpg"), b"old").unwrap();

    run(
        &manifest_for(&photos),
        &layout,
        jiff::civil::date(2026, 9, 22),
        &fetcher_for(&photos),
    )
    .unwrap();
    assert!(!layout.out.join("99-removed.jpg").exists());
}

#[test]
fn a_different_anchor_changes_only_the_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let a = jpeg(40, 30, 1);
    let photos = [("only", a.as_slice())];
    let manifest = manifest_for(&photos);
    let fetcher = fetcher_for(&photos);
    let layout = Layout::under(tmp.path());
    let file = layout.out.join("01-only.jpg");
    let d1 = run(&manifest, &layout, jiff::civil::date(2026, 9, 22), &fetcher).unwrap();
    let pixels1 = image::open(&file).unwrap().into_rgb8();
    let d2 = run(&manifest, &layout, jiff::civil::date(2026, 9, 23), &fetcher).unwrap();
    let pixels2 = image::open(&file).unwrap().into_rgb8();
    assert_ne!(d1, d2, "the embedded capture date moved");
    assert_eq!(pixels1, pixels2, "the image itself did not");
}
