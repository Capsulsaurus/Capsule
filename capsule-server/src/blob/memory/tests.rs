use super::super::conformance;
use super::*;

fn harness() -> InMemoryBlobStore {
    InMemoryBlobStore::new()
}

/// Declares one test per conformance case.
///
/// One test each, on a fresh store each: a failure names the property that broke, and no case
/// can pass because a previous one left the store in a convenient state.
macro_rules! conformance_cases {
    ($($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&harness()).await;
            }
        )+
    };
}

conformance_cases! {
    staging_appends_only_at_the_end_and_tracks_its_length,
    an_append_at_the_wrong_offset_is_refused_and_names_the_resume_point,
    truncating_a_stage_cuts_it_back_and_never_grows_it,
    appending_to_an_upload_that_was_never_begun_is_not_staged,
    abandoning_removes_the_staged_bytes_and_says_whether_there_were_any,
    the_staged_listing_is_ordered_and_holds_only_open_uploads,
    an_upload_id_that_cannot_name_a_file_is_refused_by_every_operation,
    committing_places_the_staged_bytes_at_their_content_address,
    committing_onto_an_occupied_address_keeps_the_bytes_already_there,
    committing_an_upload_that_was_never_staged_is_not_staged,
    staged_bytes_are_not_a_blob_until_they_are_committed,
    put_stores_bytes_at_its_address_and_never_overwrites,
    an_absent_address_stats_and_reads_as_none,
    a_ranged_read_returns_exactly_its_window_and_clamps_at_the_end,
    a_staged_upload_reads_back_in_windows,
    enumeration_yields_every_blob_in_content_address_order,
    enumeration_resumes_from_its_cursor_without_gaps_or_repeats,
    an_empty_store_enumerates_to_nothing_rather_than_failing,
    a_partially_populated_shard_tree_enumerates_completely,
    enumeration_reports_what_is_not_a_blob_as_debris,
    removing_a_blob_drops_it_from_lookup_and_from_enumeration,
    quarantining_pulls_a_blob_out_of_the_store_and_records_why,
    quarantining_an_absent_address_holds_nothing,
}

/// The whole suite, in one pass on one store.
///
/// The entry point an expensive-to-stand-up adapter uses, so it is exercised here too, and it
/// proves the cases really are independent: they share a store that only accumulates.
#[tokio::test]
async fn the_whole_suite_passes_in_one_pass() {
    conformance::run_all(&harness()).await;
}

/// Staged bytes are inspectable without committing them, which is what makes the double
/// useful to a test of the upload surface above the port.
#[tokio::test]
async fn a_stage_is_readable_before_it_is_committed() {
    let store = harness();
    let upload = UploadId::new("readable-stage");

    assert_eq!(store.staged_bytes(&upload), None);
    store.begin(&upload).await.expect("begin");
    store.append(&upload, 0, b"half").await.expect("append");
    assert_eq!(store.staged_bytes(&upload), Some(b"half".to_vec()));
}
