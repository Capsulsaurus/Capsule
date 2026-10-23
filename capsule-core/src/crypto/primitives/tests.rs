use super::*;

#[test]
fn suite_id_round_trip_dispatches_to_exactly_one_row() {
    assert_eq!(SuiteId::from_u16(0x0001), Some(SuiteId::V1));
    assert_eq!(SuiteId::V1.as_u16(), 0x0001);
    assert_eq!(SuiteId::CURRENT, SuiteId::V1);
    assert_eq!(SuiteId::V1.hash_len(), 32);
}

#[test]
fn unknown_suite_ids_fail_closed() {
    assert_eq!(SuiteId::from_u16(0x0000), None);
    assert_eq!(SuiteId::from_u16(0x0002), None);
    assert_eq!(SuiteId::from_u16(0xffff), None);
}

#[test]
fn device_tier_params_match_inventory() {
    assert_eq!(
        DeviceTier::LowRam.params(),
        Argon2Params {
            mem_kib: 131_072,
            t_cost: 3,
            p_cost: 1
        }
    );
    assert_eq!(
        DeviceTier::Normal.params(),
        Argon2Params {
            mem_kib: 262_144,
            t_cost: 3,
            p_cost: 1
        }
    );
    assert_eq!(
        DeviceTier::Desktop.params(),
        Argon2Params {
            mem_kib: 524_288,
            t_cost: 4,
            p_cost: 1
        }
    );
}

#[test]
fn info_labels_are_versioned_and_distinct() {
    // Distinct labels keep derived keys in separate domains.
    assert_ne!(info::ASSET_FILE_V1, info::METADATA_BLOB_V1);
    assert_ne!(info::ASSET_FILE_V1, info::DEFAULT_ALBUM_ID_V1);
    assert_ne!(info::ASSET_KEYWRAP_V1, info::ASSET_FILE_V1);
    assert_ne!(info::ASSET_KEYWRAP_V1, info::METADATA_BLOB_V1);
    assert!(info::ASSET_FILE_V1.ends_with(b"/v1"));
    assert!(info::METADATA_BLOB_V1.ends_with(b"/v1"));
    assert!(info::ASSET_KEYWRAP_V1.ends_with(b"/v1"));
}
