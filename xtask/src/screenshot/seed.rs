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
mod tests;
