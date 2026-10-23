use super::*;

#[test]
fn actions_use_the_exact_wire_strings() {
    let cases = [
        (Action::Create, "create"),
        (Action::Replace, "replace"),
        (Action::Delete, "delete"),
        (Action::MetadataUpdate, "metadata-update"),
        (Action::DerivativeAdd, "derivative-add"),
        (Action::DerivativeReplace, "derivative-replace"),
        (Action::TrashRestore, "trash-restore"),
    ];
    for (action, wire) in cases {
        let bytes = crate::cbor::to_canonical_vec(&action).unwrap();
        let decoded: Action = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(decoded, action);
        // The encoded value is exactly the kebab-case text string.
        let as_text: String = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(as_text, wire);
    }
}

#[test]
fn unknown_action_value_is_rejected() {
    // A closed enum: an unknown string fails to decode (not "ignored as future").
    let bytes = crate::cbor::to_canonical_vec(&"future-action-not-yet-defined").unwrap();
    assert!(crate::cbor::from_slice::<Action>(&bytes).is_err());
}

#[test]
fn only_create_is_a_chain_root() {
    assert!(Action::Create.is_create());
    for a in [
        Action::Replace,
        Action::Delete,
        Action::MetadataUpdate,
        Action::DerivativeAdd,
        Action::DerivativeReplace,
        Action::TrashRestore,
    ] {
        assert!(!a.is_create());
    }
}

#[test]
fn only_create_replace_and_metadata_update_bind_a_metadata_blob() {
    // The three metadata-bearing actions commit to `metadata_blob_hash`...
    for a in [Action::Create, Action::Replace, Action::MetadataUpdate] {
        assert!(a.binds_metadata_blob(), "{a:?} must bind a metadata blob");
    }
    // ...the other four omit it (key absent on the wire).
    for a in [
        Action::Delete,
        Action::DerivativeAdd,
        Action::DerivativeReplace,
        Action::TrashRestore,
    ] {
        assert!(!a.binds_metadata_blob(), "{a:?} must not bind a blob");
    }
}
