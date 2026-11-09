//! The `--json` documents `capsule federation list`, `unsubscribe` and `metadata` print: each
//! is a contract, so its keys are pinned.

use super::{PulledMetadataReport, SubscriptionsReport, UnsubscribeReport};
use crate::federation::SubscriptionEntry;

#[test]
fn the_list_document_keeps_its_keys() {
    let report = SubscriptionsReport {
        subscriptions: vec![SubscriptionEntry {
            peer: "home.test".to_owned(),
            album_id: "album-1".to_owned(),
            scope: "read-derivative-only".to_owned(),
            jti: "jti-1".to_owned(),
            issued_at: "2026-11-02T00:00:00Z".to_owned(),
            expires_at: "2026-11-03T00:00:00Z".to_owned(),
            subscribed_at: "2026-11-01T00:00:00Z".to_owned(),
            live: false,
        }],
    };
    assert_eq!(
        serde_json::to_value(&report).expect("serializes"),
        serde_json::json!({
            "subscriptions": [{
                "peer": "home.test",
                "album_id": "album-1",
                "scope": "read-derivative-only",
                "jti": "jti-1",
                "issued_at": "2026-11-02T00:00:00Z",
                "expires_at": "2026-11-03T00:00:00Z",
                "subscribed_at": "2026-11-01T00:00:00Z",
                "live": false,
            }],
        })
    );
    assert_eq!(
        serde_json::to_value(SubscriptionsReport {
            subscriptions: Vec::new(),
        })
        .expect("serializes"),
        serde_json::json!({ "subscriptions": [] }),
        "no subscription is an empty list, not an absent key"
    );
}

#[test]
fn the_unsubscribe_document_keeps_its_keys() {
    let report = UnsubscribeReport {
        peer: "home.test".to_owned(),
        album_id: "album-1".to_owned(),
        unsubscribed: true,
    };
    assert_eq!(
        serde_json::to_value(&report).expect("serializes"),
        serde_json::json!({
            "peer": "home.test",
            "album_id": "album-1",
            "unsubscribed": true,
        })
    );
}

#[test]
fn the_metadata_document_keeps_its_keys() {
    let report = PulledMetadataReport {
        peer: "home.test".to_owned(),
        album_id: "album-1".to_owned(),
        asset_id: "asset-1".to_owned(),
        size: 42,
        hash: "ab".repeat(32),
        out: std::path::PathBuf::from("out/asset-1.bin"),
    };
    assert_eq!(
        serde_json::to_value(&report).expect("serializes"),
        serde_json::json!({
            "peer": "home.test",
            "album_id": "album-1",
            "asset_id": "asset-1",
            "size": 42,
            "hash": "ab".repeat(32),
            "out": "out/asset-1.bin",
        })
    );
}
