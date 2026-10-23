//! The seed manifest (`xtask/screenshot/seed.toml`): which photos seed the README screenshot,
//! where their bytes come from, and when each one claims to have been captured.
//!
//! Capture dates are stored **relative to an anchor day** (`day_offset` days before it, at a
//! wall-clock `time`) rather than as absolute timestamps. The iOS timeline titles a day section
//! `Sat, September 12` only while it falls in the current year (and `Today` / `Yesterday` for the
//! two most recent days), so absolute dates would drift into year-suffixed headers as the calendar
//! moves on. Anchoring to the run date keeps every capture reading the same way.

use std::collections::HashSet;

use eyre::{Result, bail, ensure};
use jiff::civil::{Date, DateTime, Time};
use serde::Deserialize;

/// The smallest `day_offset` allowed: offsets 0 and 1 would render as `Today` / `Yesterday`.
pub(crate) const MIN_DAY_OFFSET: u16 = 2;

/// Licences a seed photo may carry: no attribution or share-alike obligation at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub(crate) enum License {
    #[serde(rename = "CC0-1.0")]
    Cc0,
    #[serde(rename = "PDM-1.0")]
    PublicDomainMark,
}

/// One seed photo.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Photo {
    /// Stable kebab-case id; names the normalized output file.
    pub(crate) id: String,
    /// Days before the anchor day this photo was "captured".
    pub(crate) day_offset: u16,
    /// Wall-clock capture time (`HH:MM`) on that day, in the simulator's local time zone.
    pub(crate) time: Time,
    /// Immutable original-file URL (HTTPS).
    pub(crate) url: String,
    /// Lowercase hex SHA-256 of the bytes at `url`.
    pub(crate) sha256: String,
    /// Human-facing provenance page (licence, author).
    pub(crate) source_page: String,
    pub(crate) author: String,
    pub(crate) license: License,
}

impl Photo {
    /// The capture timestamp this photo gets for a given anchor day.
    pub(crate) fn captured_at(&self, anchor: Date) -> Result<DateTime> {
        let day = anchor.checked_sub(jiff::Span::new().days(i64::from(self.day_offset)))?;
        Ok(day.to_datetime(self.time))
    }
}

/// The parsed, validated manifest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    #[serde(rename = "photo")]
    pub(crate) photos: Vec<Photo>,
}

impl Manifest {
    /// Parse and validate manifest TOML.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml_edit::de::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Require every capture day to hold a whole number of `columns`-wide grid rows, so no day
    /// section in the screenshot ends in a ragged, half-empty row.
    pub(crate) fn check_full_rows(&self, columns: usize) -> Result<()> {
        for day in self.photos.chunk_by(|a, b| a.day_offset == b.day_offset) {
            ensure!(
                day.len() % columns == 0,
                "day_offset {} holds {} photos; use a multiple of {columns} so its grid rows are full",
                day[0].day_offset,
                day.len()
            );
        }
        Ok(())
    }

    /// Photos whose capture day falls in an earlier calendar year than `today` for this `anchor`.
    /// The timeline titles those sections with a year suffix (`December 30, 2025`), which a run in
    /// the first days of January would put into the screenshot.
    pub(crate) fn previous_year_photos(&self, anchor: Date, today: Date) -> Result<Vec<&Photo>> {
        let mut out = Vec::new();
        for photo in &self.photos {
            if photo.captured_at(anchor)?.year() < today.year() {
                out.push(photo);
            }
        }
        Ok(out)
    }

    /// Reject anything that would make the seed set ambiguous or non-reproducible.
    ///
    /// Photos must be listed **newest first** — the order the timeline renders them — so the
    /// manifest reads top-to-bottom like the screenshot: `day_offset` never decreases, and within
    /// one day `time` strictly decreases.
    fn validate(&self) -> Result<()> {
        ensure!(!self.photos.is_empty(), "manifest lists no photos");
        let mut ids = HashSet::new();
        let mut hashes = HashSet::new();
        let mut previous: Option<&Photo> = None;
        for photo in &self.photos {
            let id = &photo.id;
            ensure!(is_kebab(id), "photo id `{id}` is not kebab-case");
            ensure!(ids.insert(id.as_str()), "duplicate photo id `{id}`");
            ensure!(
                is_sha256_hex(&photo.sha256),
                "photo `{id}`: sha256 must be 64 lowercase hex characters"
            );
            ensure!(
                hashes.insert(photo.sha256.as_str()),
                "photo `{id}`: duplicate sha256 (the same file is listed twice)"
            );
            ensure!(
                photo.url.starts_with("https://"),
                "photo `{id}`: url must be https"
            );
            ensure!(
                photo.day_offset >= MIN_DAY_OFFSET,
                "photo `{id}`: day_offset must be >= {MIN_DAY_OFFSET} (0 and 1 render as Today/Yesterday)"
            );
            if let Some(prev) = previous {
                let newer_first = prev.day_offset < photo.day_offset
                    || (prev.day_offset == photo.day_offset && prev.time > photo.time);
                if !newer_first {
                    bail!(
                        "photo `{id}` is not older than `{}`: list photos newest first with distinct times",
                        prev.id
                    );
                }
            }
            previous = Some(photo);
        }
        Ok(())
    }
}

fn is_kebab(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
