//! Locale negotiation: pick the best supported locale for a request.

use std::cmp::Ordering;

/// Choose the best `supported` locale for an `Accept-Language`-style request
/// string, falling back to `source` when nothing matches.
///
/// The request is parsed as a comma-separated list of BCP-47 tags with optional
/// `;q=` weights (so an HTTP `Accept-Language` header or a browser
/// `navigator.language` value both work). Tags are tried highest-weight first,
/// matching a supported locale exactly, then by primary language subtag
/// (`fr-CA` matches supported `fr`). Matching is case-insensitive.
#[must_use]
pub fn negotiate(accept_language: &str, supported: &[&str], source: &str) -> String {
    for tag in requested_tags(accept_language) {
        if let Some(matched) = supported.iter().find(|s| s.eq_ignore_ascii_case(&tag)) {
            return (*matched).to_string();
        }
        let primary = primary_subtag(&tag);
        if let Some(matched) = supported.iter().find(|s| primary_subtag(s) == primary) {
            return (*matched).to_string();
        }
    }
    source.to_string()
}

/// The lowercased primary language subtag of `tag` (the part before `-`/`_`).
fn primary_subtag(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

/// Parse an `Accept-Language`-style string into lowercased tags, highest weight
/// first (ties keep their original order).
fn requested_tags(accept_language: &str) -> Vec<String> {
    let mut items: Vec<(f32, usize, String)> = accept_language
        .split(',')
        .enumerate()
        .filter_map(|(index, part)| {
            let mut fields = part.trim().split(';');
            let tag = fields.next()?.trim().to_ascii_lowercase();
            if tag.is_empty() || tag == "*" {
                return None;
            }
            let weight = fields
                .find_map(|f| f.trim().strip_prefix("q="))
                .and_then(|q| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            Some((weight, index, tag))
        })
        .collect();
    items.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    items.into_iter().map(|(_, _, tag)| tag).collect()
}

#[cfg(test)]
mod tests {
    use super::negotiate;

    const SUPPORTED: &[&str] = &["en", "fr", "de"];

    #[test]
    fn exact_match_wins() {
        assert_eq!(negotiate("fr", SUPPORTED, "en"), "fr");
    }

    #[test]
    fn primary_subtag_matches_region() {
        assert_eq!(negotiate("fr-CA", SUPPORTED, "en"), "fr");
    }

    #[test]
    fn highest_weight_is_preferred() {
        assert_eq!(negotiate("fr;q=0.5, de;q=0.9", SUPPORTED, "en"), "de");
    }

    #[test]
    fn unmatched_falls_back_to_source() {
        assert_eq!(negotiate("es-MX, es;q=0.9", SUPPORTED, "en"), "en");
    }

    #[test]
    fn empty_request_falls_back_to_source() {
        assert_eq!(negotiate("", SUPPORTED, "en"), "en");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(negotiate("FR-fr", SUPPORTED, "en"), "fr");
    }
}
