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
    let manifest = Manifest::parse(include_str!("../../../screenshot/seed.toml")).unwrap();
    manifest
        .check_full_rows(super::super::HERO_COLUMNS)
        .unwrap();
    assert!(
        manifest.photos.len() >= 30,
        "enough photos to fill the scrolled grid"
    );
}
