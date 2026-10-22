//! The pure half of re-announce: which held assets to send, how they are batched, and how an
//! answer decodes.

use super::*;

fn held(id: &str, size: usize) -> HeldManifest {
    HeldManifest {
        asset_id: id.to_owned(),
        manifest: vec![0; size],
    }
}

#[test]
fn only_what_the_feed_does_not_list_is_reannounced_in_the_order_held() {
    let held = [held("a", 1), held("b", 1), held("c", 1)];
    let listed: HashSet<String> = HashSet::from(["b".to_owned()]);
    let ids: Vec<&str> = unlisted(&held, &listed)
        .iter()
        .map(|manifest| manifest.asset_id.as_str())
        .collect();
    assert_eq!(ids, ["a", "c"]);
}

#[test]
fn nothing_is_reannounced_when_the_feed_lists_everything() {
    let held = [held("a", 1)];
    let listed: HashSet<String> = HashSet::from(["a".to_owned()]);
    assert!(unlisted(&held, &listed).is_empty());
    assert!(batches(&[]).is_empty());
}

#[test]
fn batches_are_cut_by_count() {
    let held: Vec<HeldManifest> = (0..=MAX_BATCH_MANIFESTS * 2)
        .map(|n| held(&n.to_string(), 10))
        .collect();
    let refs: Vec<&HeldManifest> = held.iter().collect();
    let sizes: Vec<usize> = batches(&refs).iter().map(Vec::len).collect();
    assert_eq!(sizes, [MAX_BATCH_MANIFESTS, MAX_BATCH_MANIFESTS, 1]);
}

#[test]
fn batches_are_cut_by_bytes_and_an_oversized_manifest_rides_alone() {
    let half = MAX_BATCH_BYTES / 2;
    let held = [
        held("a", half),
        held("b", half),
        held("c", 1),
        held("huge", MAX_BATCH_BYTES + 1),
        held("d", 1),
    ];
    let refs: Vec<&HeldManifest> = held.iter().collect();
    let ids: Vec<Vec<&str>> = batches(&refs)
        .iter()
        .map(|batch| batch.iter().map(|m| m.asset_id.as_str()).collect())
        .collect();
    assert_eq!(
        ids,
        vec![vec!["a", "b"], vec!["c"], vec!["huge"], vec!["d"]]
    );
}

#[test]
fn a_wire_answer_decodes_into_its_outcome() {
    let restored = decode_result(rest::types::ReannounceResult {
        asset_id: Some("a".to_owned()),
        outcome: rest::types::ReannounceOutcome::Restored,
        sync_seq: Some(7),
        missing: None,
        code: None,
    });
    assert_eq!(
        restored.outcome,
        ReannounceOutcome::Restored { sync_seq: Some(7) }
    );
    let missing = decode_result(rest::types::ReannounceResult {
        asset_id: Some("b".to_owned()),
        outcome: rest::types::ReannounceOutcome::MissingBlobs,
        sync_seq: None,
        missing: Some(vec!["ab".to_owned()]),
        code: None,
    });
    assert_eq!(
        missing.outcome,
        ReannounceOutcome::MissingBlobs(vec!["ab".to_owned()])
    );
    let refused = decode_result(rest::types::ReannounceResult {
        asset_id: None,
        outcome: rest::types::ReannounceOutcome::Refused,
        sync_seq: None,
        missing: None,
        code: Some("error.sync.reannounce_malformed".to_owned()),
    });
    assert_eq!(
        refused.outcome,
        ReannounceOutcome::Refused {
            code: "error.sync.reannounce_malformed".to_owned()
        }
    );
}
