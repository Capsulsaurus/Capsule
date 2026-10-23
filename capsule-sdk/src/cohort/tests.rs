use super::*;

#[test]
fn linux_reader_hashes_machine_id_never_returns_raw() {
    let dir = std::env::temp_dir().join(format!("capsule-cohort-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("machine-id");
    let raw = "abcdef0123456789abcdef0123456789";
    std::fs::write(&path, format!("{raw}\n")).unwrap();

    let reader = LinuxMachineIdReader::from_path(&path);
    assert_eq!(reader.platform(), PlatformTag::Linux);
    let id = reader.read_primary_id().unwrap();

    // The returned primary_id is the derived hash (64 hex chars), never the raw
    // machine-id — the stable device secret must not leave the device.
    assert_ne!(id, raw);
    assert_eq!(id.len(), 64);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    // Deterministic derivation.
    assert_eq!(
        id,
        LinuxMachineIdReader::from_path(&path)
            .read_primary_id()
            .unwrap()
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn linux_reader_reports_empty_and_unavailable_cleanly() {
    let dir = std::env::temp_dir().join(format!("capsule-cohort-e-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("machine-id");
    std::fs::write(&empty, "\n").unwrap();
    assert!(matches!(
        LinuxMachineIdReader::from_path(&empty).read_primary_id(),
        Err(IdentifierError::Empty)
    ));
    assert!(matches!(
        LinuxMachineIdReader::from_path(dir.join("does-not-exist")).read_primary_id(),
        Err(IdentifierError::Unavailable)
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn compute_cohort_hash_matches_core_and_is_deterministic() {
    let dir = std::env::temp_dir().join(format!("capsule-cohort-c-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("machine-id");
    std::fs::write(&path, "0011223344556677\n").unwrap();
    let reader = LinuxMachineIdReader::from_path(&path);
    let user = Uuid::from_u128(0xABCD);

    let a = compute_cohort_hash(&reader, user).unwrap();
    let b = compute_cohort_hash(&reader, user).unwrap();
    assert_eq!(a, b, "same device + account ⇒ same cohort");

    // Byte-identical to computing through the core hash directly.
    let primary = reader.read_primary_id().unwrap();
    let expected = cohort_hash(user, PlatformTag::Linux, &primary).to_hex();
    assert_eq!(a, expected);

    // A different account on the same device ⇒ a different (unlinkable) hash.
    let other = compute_cohort_hash(&reader, Uuid::from_u128(0x1234)).unwrap();
    assert_ne!(a, other);

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(target_os = "macos")]
#[test]
fn macos_reader_reads_a_uuid_shaped_platform_id() {
    let reader = MacosPlatformUuidReader::new();
    assert_eq!(reader.platform(), PlatformTag::Macos);
    let id = reader
        .read_primary_id()
        .expect("IOPlatformUUID on macOS host");
    // IOPlatformUUID is a canonical UUID string; assert it parses as one.
    assert!(
        uuid::Uuid::parse_str(&id).is_ok(),
        "IOPlatformUUID parses as a UUID"
    );
    // Folds into a stable cohort hash.
    let user = Uuid::from_u128(7);
    assert_eq!(
        compute_cohort_hash(&reader, user).unwrap(),
        cohort_hash(user, PlatformTag::Macos, &id).to_hex()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_parse_uuid_extracts_the_value() {
    let sample = "    \"IOPlatformUUID\" = \"12345678-90AB-CDEF-1234-567890ABCDEF\"\n";
    assert_eq!(
        MacosPlatformUuidReader::parse_uuid(sample).as_deref(),
        Some("12345678-90AB-CDEF-1234-567890ABCDEF")
    );
    assert_eq!(MacosPlatformUuidReader::parse_uuid("no uuid here"), None);
}
