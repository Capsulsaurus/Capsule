use super::*;

#[test]
fn full_history_entitles_the_whole_range() {
    assert_eq!(HistoryPolicy::Full.entitled_range(1), 1..=1);
    assert_eq!(HistoryPolicy::Full.entitled_range(5), 1..=5);
}

#[test]
fn capped_history_entitles_only_the_last_n_epochs() {
    // Last 2 epochs at ceiling 5 → {4, 5}.
    assert_eq!(HistoryPolicy::Capped(2).entitled_range(5), 4..=5);
    // Cap wider than history → whole range (equivalent to Full).
    assert_eq!(HistoryPolicy::Capped(10).entitled_range(3), 1..=3);
    // Cap of 1 → current epoch only.
    assert_eq!(HistoryPolicy::Capped(1).entitled_range(7), 7..=7);
    // Cap of 0 floors to the current epoch (never an empty range).
    assert_eq!(HistoryPolicy::Capped(0).entitled_range(7), 7..=7);
}

#[test]
fn entitled_range_is_a_pure_function_of_policy_and_ceiling() {
    // The consistency invariant: identical (policy, ceiling) ⇒ identical range, regardless of
    // any per-add context — there is no per-add input to this function at all.
    for ceiling in 1..=8u32 {
        for policy in [HistoryPolicy::Full, HistoryPolicy::Capped(3)] {
            assert_eq!(
                policy.entitled_range(ceiling),
                policy.entitled_range(ceiling)
            );
        }
    }
}

#[test]
fn app_payload_round_trips_through_canonical_cbor() {
    let kd = MlsAppPayload::KeyDistribution(AlbumKeyDistribution {
        amk_version: 3,
        amk_bytes: [7u8; AMK_LEN],
    });
    let bytes = kd.to_bytes().unwrap();
    assert_eq!(MlsAppPayload::from_bytes(&bytes).unwrap(), kd);
}
