//! `xtask screenshot-seed`: turn the manifest into a directory of upright, dated, metadata-free
//! JPEGs ready for `simctl addmedia`.
//!
//! Two stages, each idempotent:
//!
//! 1. **Cache.** Every original is fetched once into `target/screenshot/cache/<sha256>.jpg` and
//!    verified against the manifest's pin; a verified cache entry is never fetched again, and bytes
//!    that fail the pin are deleted and reported rather than used.
//! 2. **Normalize.** Each original is decoded, rotated upright, downscaled to [`MAX_EDGE`] and
//!    re-encoded with only a capture date ([`super::capture_exif`]) and its ICC profile. The same
//!    manifest, anchor day and originals always produce byte-identical files; the whole directory
//!    is rebuilt in a sibling and swapped in, so a failed run never leaves a half-written seed set.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use eyre::{Context, Result, bail, ensure, eyre};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};
use jiff::civil::Date;
use sha2::{Digest, Sha256};
use tracing::{debug, info, info_span, warn};

use super::capture_exif;
use super::manifest::{Manifest, Photo};

/// Longest edge of a normalized seed photo, in pixels. The grid shows ~400 px tiles at 3x and the
/// viewer fills a 1206 px-wide screen, so 2048 keeps full-screen detail at a fraction of the bytes.
pub(crate) const MAX_EDGE: u32 = 2048;

/// Fixed JPEG quality for re-encoding.
const JPEG_QUALITY: u8 = 90;

/// Identifies this tool to the file host, per the Wikimedia User-Agent policy.
const USER_AGENT: &str = "CapsuleScreenshotSeed/1.0 (https://github.com/Capsulsaurus/Capsule)";

/// Downloads one URL to a local path. A seam so the cache logic is testable offline.
pub(crate) trait Fetcher: Sync {
    fn fetch(&self, url: &str, dest: &Path) -> Result<()>;
}

/// Production fetcher: `curl`, which ships with macOS and every Linux dev box, retries transient
/// failures (including HTTP 429 with `Retry-After`) and keeps an HTTP stack out of xtask.
pub(crate) struct CurlFetcher;

impl Fetcher for CurlFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> Result<()> {
        let status = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args([
                "--retry",
                "6",
                "--retry-delay",
                "5",
                "--connect-timeout",
                "20",
            ])
            .args(["--user-agent", USER_AGENT, "--output"])
            .arg(dest)
            .arg(url)
            .status()
            .context("running curl (is it installed?)")?;
        ensure!(status.success(), "curl exited with {status} for {url}");
        Ok(())
    }
}

/// Whether [`ensure_cached`] had to download.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CacheOutcome {
    Hit,
    Fetched,
}

/// Paths the seed stage reads and writes.
pub(crate) struct Layout {
    pub(crate) cache: PathBuf,
    pub(crate) out: PathBuf,
}

impl Layout {
    pub(crate) fn under(target: &Path) -> Self {
        Self {
            cache: target.join("cache"),
            out: target.join("seed"),
        }
    }
}

/// Run both stages. Returns the combined SHA-256 of the output files (in manifest order), which a
/// second run must reproduce exactly — the idempotency check the mise task prints.
pub(crate) fn run(
    manifest: &Manifest,
    layout: &Layout,
    anchor: Date,
    fetcher: &dyn Fetcher,
) -> Result<String> {
    let started = Instant::now();
    fs::create_dir_all(&layout.cache)
        .with_context(|| format!("creating {}", layout.cache.display()))?;

    // Fetch sequentially: politeness to the file host matters more than wall-clock here, and a
    // warm cache makes this loop free.
    let mut originals = Vec::with_capacity(manifest.photos.len());
    for photo in &manifest.photos {
        let (path, outcome) = ensure_cached(photo, &layout.cache, fetcher)?;
        debug!(id = %photo.id, ?outcome, "original ready");
        originals.push(path);
    }

    let staging = layout.out.with_extension("staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
    }
    fs::create_dir_all(&staging)?;

    let names = normalize_all(manifest, &originals, &staging, anchor)?;

    if layout.out.exists() {
        fs::remove_dir_all(&layout.out)
            .with_context(|| format!("replacing {}", layout.out.display()))?;
    }
    fs::rename(&staging, &layout.out)?;

    let mut combined = Sha256::new();
    for name in &names {
        combined.update(name.as_bytes());
        combined.update(Sha256::digest(fs::read(layout.out.join(name))?));
    }
    let digest = hex::encode(combined.finalize());
    info!(
        photos = names.len(),
        out = %layout.out.display(),
        %anchor,
        seed_digest = %digest,
        elapsed_ms = started.elapsed().as_millis(),
        "seed set ready"
    );
    Ok(digest)
}

/// Make sure the verified original for `photo` is in the cache; return its path.
pub(crate) fn ensure_cached(
    photo: &Photo,
    cache: &Path,
    fetcher: &dyn Fetcher,
) -> Result<(PathBuf, CacheOutcome)> {
    let path = cache.join(format!("{}.jpg", photo.sha256));
    if path.exists() {
        if sha256_file(&path)? == photo.sha256 {
            return Ok((path, CacheOutcome::Hit));
        }
        warn!(id = %photo.id, path = %path.display(), "cached original fails its pin; refetching");
        fs::remove_file(&path)?;
    }

    let partial = path.with_extension("part");
    let span = info_span!("fetch", id = %photo.id, url = %photo.url);
    let _guard = span.enter();
    let started = Instant::now();
    fetcher
        .fetch(&photo.url, &partial)
        .with_context(|| format!("fetching photo `{}`", photo.id))?;
    let actual = sha256_file(&partial)?;
    if actual != photo.sha256 {
        fs::remove_file(&partial)?;
        bail!(
            "photo `{}`: downloaded bytes do not match the manifest pin (expected {}, got {actual}); \
             the source changed — review it and re-pin the manifest",
            photo.id,
            photo.sha256
        );
    }
    fs::rename(&partial, &path)?;
    info!(
        bytes = fs::metadata(&path)?.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "fetched"
    );
    Ok((path, CacheOutcome::Fetched))
}

/// Normalize every original into `dir` in parallel; returns the output file names in manifest order.
fn normalize_all(
    manifest: &Manifest,
    originals: &[PathBuf],
    dir: &Path,
    anchor: Date,
) -> Result<Vec<String>> {
    let jobs: Vec<(usize, &Photo, &PathBuf)> = manifest
        .photos
        .iter()
        .zip(originals)
        .enumerate()
        .map(|(i, (p, o))| (i, p, o))
        .collect();
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = jobs.len().div_ceil(workers).max(1);

    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|batch| {
                scope.spawn(move || -> Result<Vec<(usize, String)>> {
                    batch
                        .iter()
                        .map(|&(i, photo, original)| {
                            let started = Instant::now();
                            let name = output_name(i, photo);
                            let bytes = fs::read(original)?;
                            let jpeg = normalize(&bytes, photo.captured_at(anchor)?)
                                .with_context(|| format!("normalizing photo `{}`", photo.id))?;
                            fs::write(dir.join(&name), &jpeg)?;
                            debug!(
                                id = %photo.id,
                                bytes = jpeg.len(),
                                elapsed_ms = started.elapsed().as_millis(),
                                "normalized"
                            );
                            Ok((i, name))
                        })
                        .collect()
                })
            })
            .collect();
        let mut names = Vec::with_capacity(jobs.len());
        for handle in handles {
            names.extend(
                handle
                    .join()
                    .map_err(|_| eyre!("normalize worker panicked"))??,
            );
        }
        names.sort_unstable();
        Ok(names.into_iter().map(|(_, n)| n).collect())
    })
}

/// `NN-<id>.jpg`, numbered in manifest (newest-first) order.
pub(crate) fn output_name(index: usize, photo: &Photo) -> String {
    format!("{:02}-{}.jpg", index + 1, photo.id)
}

/// Decode, orient upright, bound the long edge, and re-encode with only a capture date.
pub(crate) fn normalize(original: &[u8], captured_at: jiff::civil::DateTime) -> Result<Vec<u8>> {
    let mut decoder = ImageReader::new(Cursor::new(original))
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let icc = decoder.icc_profile()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);

    let (w, h) = (image.width(), image.height());
    if w.max(h) > MAX_EDGE {
        // Round to the nearest pixel so the aspect ratio survives the downscale.
        let scale = f64::from(MAX_EDGE) / f64::from(w.max(h));
        let nw = ((f64::from(w) * scale).round() as u32).max(1);
        let nh = ((f64::from(h) * scale).round() as u32).max(1);
        image = image.resize_exact(nw, nh, FilterType::Lanczos3);
    }
    let rgb = image.into_rgb8();

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    // Pixels are RGB now; a Gray or CMYK profile from the source would misdescribe them.
    if let Some(icc) = icc.filter(|p| is_rgb_profile(p)) {
        encoder
            .set_icc_profile(icc)
            .map_err(|e| eyre!("embedding ICC profile: {e}"))?;
    }
    encoder
        .set_exif_metadata(capture_exif::capture_date_block(captured_at))
        .map_err(|e| eyre!("embedding EXIF: {e}"))?;
    encoder.write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// Whether an ICC profile describes RGB data (header bytes 16..20, the data colour space).
pub(crate) fn is_rgb_profile(profile: &[u8]) -> bool {
    profile.get(16..20) == Some(b"RGB ".as_slice())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
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
        let mut exif =
            capture_exif::capture_date_block(jiff::civil::date(2020, 1, 1).at(0, 0, 0, 0));
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
}
