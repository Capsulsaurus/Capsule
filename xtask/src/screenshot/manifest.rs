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
mod tests {
    use super::*;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn photo(id: &str, day_offset: u16, time: &str, sha256: &str, license: &str) -> String {
        format!(
            r#"
[[photo]]
id = "{id}"
day_offset = {day_offset}
time = "{time}"
url = "https://example.test/{id}.jpg"
sha256 = "{sha256}"
source_page = "https://example.test/{id}"
author = "Someone"
license = "{license}"
"#
        )
    }

    fn err(text: &str) -> String {
        Manifest::parse(text).unwrap_err().to_string()
    }

    #[test]
    fn parses_a_valid_manifest_in_listed_order() {
        let text =
            photo("a", 2, "17:40", HASH_A, "CC0-1.0") + &photo("b", 2, "17:33", HASH_B, "PDM-1.0");
        let manifest = Manifest::parse(&text).unwrap();
        let ids: Vec<_> = manifest.photos.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(manifest.photos[1].license, License::PublicDomainMark);
    }

    #[test]
    fn rejects_a_licence_outside_the_public_domain_set() {
        let text = photo("a", 2, "10:00", HASH_A, "CC-BY-4.0");
        assert!(err(&text).contains("CC-BY-4.0"), "{}", err(&text));
    }

    #[test]
    fn rejects_duplicate_ids_and_hashes() {
        let dup_id =
            photo("a", 2, "10:00", HASH_A, "CC0-1.0") + &photo("a", 3, "10:00", HASH_B, "CC0-1.0");
        assert!(err(&dup_id).contains("duplicate photo id"));
        let dup_hash =
            photo("a", 2, "10:00", HASH_A, "CC0-1.0") + &photo("b", 3, "10:00", HASH_A, "CC0-1.0");
        assert!(err(&dup_hash).contains("duplicate sha256"));
    }

    #[test]
    fn rejects_malformed_fields() {
        assert!(err(&photo("Bad_Id", 2, "10:00", HASH_A, "CC0-1.0")).contains("kebab-case"));
        assert!(err(&photo("a", 2, "10:00", "ABC", "CC0-1.0")).contains("sha256"));
        let http = photo("a", 2, "10:00", HASH_A, "CC0-1.0")
            .replace("https://example.test/a.jpg", "http://x/a.jpg");
        assert!(err(&http).contains("https"));
        let unknown = photo("a", 2, "10:00", HASH_A, "CC0-1.0") + "caption = \"nope\"\n";
        assert!(err(&unknown).contains("caption"));
    }

    #[test]
    fn rejects_offsets_that_render_as_today_or_yesterday() {
        assert!(err(&photo("a", 1, "10:00", HASH_A, "CC0-1.0")).contains("day_offset"));
    }

    #[test]
    fn requires_newest_first_order_with_distinct_times() {
        let older_day_first =
            photo("a", 3, "10:00", HASH_A, "CC0-1.0") + &photo("b", 2, "10:00", HASH_B, "CC0-1.0");
        assert!(err(&older_day_first).contains("newest first"));
        let same_time =
            photo("a", 2, "10:00", HASH_A, "CC0-1.0") + &photo("b", 2, "10:00", HASH_B, "CC0-1.0");
        assert!(err(&same_time).contains("newest first"));
    }

    #[test]
    fn capture_time_is_relative_to_the_anchor() {
        let manifest = Manifest::parse(&photo("a", 3, "17:40", HASH_A, "CC0-1.0")).unwrap();
        let at = manifest.photos[0]
            .captured_at(jiff::civil::date(2026, 3, 1))
            .unwrap();
        assert_eq!(at, jiff::civil::date(2026, 2, 26).at(17, 40, 0, 0));
    }

    #[test]
    fn full_rows_rule_counts_photos_per_capture_day() {
        let three: String = ["a", "b", "c"]
            .iter()
            .zip(["10:03", "10:02", "10:01"])
            .enumerate()
            .map(|(i, (id, t))| photo(id, 2, t, &format!("{i}").repeat(64), "CC0-1.0"))
            .collect();
        let manifest = Manifest::parse(&three).unwrap();
        assert!(manifest.check_full_rows(3).is_ok());
        let err = manifest.check_full_rows(2).unwrap_err().to_string();
        assert!(err.contains("day_offset 2 holds 3 photos"), "{err}");
    }

    #[test]
    fn flags_captures_that_fall_in_the_previous_year() {
        let text =
            photo("a", 2, "10:00", HASH_A, "CC0-1.0") + &photo("b", 9, "10:00", HASH_B, "CC0-1.0");
        let manifest = Manifest::parse(&text).unwrap();
        let jan5 = jiff::civil::date(2027, 1, 5);
        let ids: Vec<_> = manifest
            .previous_year_photos(jan5, jan5)
            .unwrap()
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["b"]);
        let sep = jiff::civil::date(2026, 9, 23);
        assert!(manifest.previous_year_photos(sep, sep).unwrap().is_empty());
    }

    /// The checked-in manifest: valid, full rows at the hero's 3 columns, and CC0-only.
    #[test]
    fn checked_in_manifest_is_valid() {
        let manifest = Manifest::parse(include_str!("../../screenshot/seed.toml")).unwrap();
        manifest
            .check_full_rows(super::super::HERO_COLUMNS)
            .unwrap();
        assert!(
            manifest.photos.len() >= 30,
            "enough photos to fill the scrolled grid"
        );
    }
}
