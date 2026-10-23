use super::*;

#[test]
fn recovery_verification_tag_compare() {
    let master = [0x42u8; 32];
    // Fast params for the test.
    let blob = pwkdf::wrap_with(
        &master,
        b"correct horse battery staple",
        crate::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    // Correct secret against the matching device master → Verified.
    assert_eq!(
        verify_recovery_secret(&blob, b"correct horse battery staple", &master),
        VerifyOutcome::Verified
    );
    // Wrong secret → NotVerified (caller applies the refresh-then-retry rule).
    assert_eq!(
        verify_recovery_secret(&blob, b"wrong", &master),
        VerifyOutcome::NotVerified
    );
    // Right secret, different device master (rotated elsewhere) → NotVerified.
    assert_eq!(
        verify_recovery_secret(&blob, b"correct horse battery staple", &[0x43u8; 32]),
        VerifyOutcome::NotVerified
    );
}

#[test]
fn master_key_escrow_round_trip() {
    let master = [0x42u8; 32];
    // Fast params for the test.
    let blob = pwkdf::wrap_with(
        &master,
        b"recovery passphrase",
        crate::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    assert_eq!(
        recover_master_key(&blob, b"recovery passphrase").unwrap(),
        master
    );
    assert!(recover_master_key(&blob, b"wrong").is_err());
}

#[test]
fn shamir_2_of_3_reconstructs_from_any_two() {
    let seed = [0x7u8; 32];
    let shares = split_seed_2of3(&seed);
    assert_eq!(shares.len(), 3);

    // Any 2 of the 3 reconstruct the seed.
    for pair in [[0, 1], [0, 2], [1, 2]] {
        let subset = vec![shares[pair[0]].clone(), shares[pair[1]].clone()];
        assert_eq!(recover_seed(&subset).unwrap(), seed);
    }
}

/// Shares `sharks` 0.5.0 issued for the seed `[0, 1, …, 31]`, captured before the swap to
/// `blahaj`. They pin the wire-format decision: shares already handed out stay recoverable.
const SHARKS_ISSUED: [[u8; SHAMIR_SHARE_LEN]; 3] = [
    [
        0x01, 0xa1, 0x62, 0x17, 0xb5, 0x53, 0xf7, 0xdb, 0x50, 0x81, 0x0e, 0x9e, 0x13, 0xdf, 0x78,
        0x76, 0xa8, 0x44, 0x35, 0xeb, 0x3a, 0x8a, 0xce, 0xf9, 0x81, 0x0f, 0x98, 0x87, 0x6b, 0x39,
        0x05, 0x1f, 0xe9,
    ],
    [
        0x02, 0x5f, 0xc7, 0x28, 0x72, 0xaa, 0xfc, 0xa1, 0xa9, 0x07, 0x07, 0x3f, 0x3b, 0xb7, 0xe7,
        0xfe, 0x5c, 0xb8, 0x59, 0xfd, 0x41, 0x35, 0xbe, 0xd5, 0x26, 0x36, 0x06, 0x3d, 0xfb, 0x56,
        0x2d, 0x1c, 0xee,
    ],
    [
        0x03, 0xfe, 0xa4, 0x3d, 0xc4, 0xfd, 0x0e, 0x7c, 0xfe, 0x8e, 0x00, 0xab, 0x23, 0x64, 0x92,
        0x86, 0xfb, 0xec, 0x7d, 0x04, 0x68, 0xab, 0x65, 0x3a, 0xb0, 0x21, 0x87, 0xa0, 0x8b, 0x73,
        0x35, 0x1d, 0x18,
    ],
];

#[test]
fn shamir_recovers_shares_issued_by_sharks() {
    let seed: Vec<u8> = (0..32).collect();
    for pair in [[0, 1], [0, 2], [1, 2], [2, 0]] {
        let subset = vec![
            SHARKS_ISSUED[pair[0]].to_vec(),
            SHARKS_ISSUED[pair[1]].to_vec(),
        ];
        assert_eq!(recover_seed(&subset).unwrap(), seed, "pair {pair:?}");
    }
    let all: Vec<Vec<u8>> = SHARKS_ISSUED.iter().map(|s| s.to_vec()).collect();
    assert_eq!(recover_seed(&all).unwrap(), seed);
}

#[test]
fn shamir_shares_use_the_documented_encoding() {
    let shares = split_seed_2of3(&[0x5u8; 32]);
    for (i, share) in shares.iter().enumerate() {
        assert_eq!(share.len(), SHAMIR_SHARE_LEN);
        assert_eq!(usize::from(share[0]), i + 1, "x runs 1, 2, 3");
    }
}

/// RUSTSEC-2024-0398: `sharks` drew the degree-1 coefficient from `[1, 255]`, so share
/// `x = 1` (`y = a + s`) never equalled the seed byte. Uniform coefficients hit it at rate
/// 1/256; over 8192 bytes the chance of never hitting it is about e^-32.
#[test]
fn shamir_coefficients_include_zero() {
    let seed = [0u8; 32];
    let hit = (0..256).any(|_| split_seed_2of3(&seed)[0][1..].contains(&0));
    assert!(
        hit,
        "a degree-1 coefficient of 0 never occurred: biased dealer"
    );
}

#[test]
fn shamir_single_share_does_not_reconstruct() {
    let shares = split_seed_2of3(&[0x9u8; 32]);
    for share in &shares {
        assert!(matches!(
            recover_seed(std::slice::from_ref(share)),
            Err(BackupError::Format(_))
        ));
    }
    assert!(matches!(recover_seed(&[]), Err(BackupError::Format(_))));
}

#[test]
fn shamir_rejects_malformed_shares() {
    let shares = split_seed_2of3(&[0x3u8; 32]);
    let good = shares[1].clone();
    let mut x_zero = shares[0].clone();
    x_zero[0] = 0;
    let cases: [(&str, Vec<Vec<u8>>); 5] = [
        ("empty share", vec![Vec::new(), good.clone()]),
        ("x only", vec![vec![1], good.clone()]),
        ("truncated", vec![shares[0][..32].to_vec(), good.clone()]),
        (
            "overlong",
            vec![[shares[0].as_slice(), &[0]].concat(), good.clone()],
        ),
        ("x = 0", vec![x_zero, good.clone()]),
    ];
    for (name, input) in cases {
        assert!(
            matches!(recover_seed(&input), Err(BackupError::Format(_))),
            "{name} must be refused"
        );
    }
}

#[test]
fn shamir_rejects_a_repeated_evaluation_point() {
    let shares = split_seed_2of3(&[0x4u8; 32]);
    // The same share twice is one share: below threshold.
    let twice = vec![shares[0].clone(), shares[0].clone()];
    assert!(matches!(recover_seed(&twice), Err(BackupError::Format(_))));
    // A forged share at an existing x would otherwise interpolate a wrong seed.
    let mut forged = shares[0].clone();
    forged[1] ^= 0xff;
    let colliding = vec![shares[0].clone(), forged, shares[1].clone()];
    assert!(matches!(
        recover_seed(&colliding),
        Err(BackupError::Format(_))
    ));
}
