//! The one suite every [`DropStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **Caps are decided and reserved together.** An admission reserves exactly one file and
//!   its bytes; every refusal — [`Admission::NotLive`], [`Admission::FileTooLarge`],
//!   [`Admission::CapExhausted`] — reserves nothing. The race itself needs two processes, so the
//!   suite asserts its consequence: a cap is never exceeded and a refusal never moves a total.
//! - **Refusals are ordered.** Unknown, expired, revoked and spent are one answer; a file the
//!   link could never accept is `FileTooLarge` before any cumulative cap is consulted.
//! - **A refund gives back what an unused reservation took**, and never drives a total below
//!   zero.
//! - **Adoption is claimed, not taken.** A claim holds the row once; a second claim, another
//!   owner's claim, or a claim of a held row is `None`; release puts it back and settle removes
//!   it.
//! - **The inbox is the owner's, oldest first**, ties broken by id, and never shows another
//!   account's drops.
//! - **A held opaque id is never re-provisioned.** Provisioning under an id already held is
//!   [`LinkProvisionOutcome::Conflict`] — from another account and from the owner alike, spent or
//!   revoked — and the held link, its totals included, reads back unchanged (#553).
//! - **Other writes under a held key replace.** Re-declaring an upload session and
//!   re-depositing a drop id each replace the earlier row whole, as the in-memory map inserts do.
//! - **Instants are not moved.** They may lose sub-microsecond precision and nothing more.
//!
//! # Reusing a harness
//!
//! Every case scopes its own opaque ids, drop ids, upload ids and accounts, so cases may share
//! one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{
    Admission, DropStore, InboxEntry, LinkCaps, LinkProvisionOutcome, PendingDeposit,
    UploadLinkRecord,
};
use crate::blob::ContentAddress;
use crate::store::{StoreError, UploadId, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The drop store under test.
    fn drops(&self) -> &dyn DropStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming drop store must succeed at {doing}: {error}"),
    }
}

/// A 32-hex opaque id unique to `case` and `n`.
fn opaque(case: &str, n: u8) -> String {
    let digest = capsule_core::crypto::hash::hash_bytes(format!("{case}/{n}").as_bytes());
    digest.to_hex()[..super::OPAQUE_ID_HEX_LEN].to_owned()
}

/// A content address derived from `seed`.
fn address(seed: &str) -> ContentAddress {
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(seed.as_bytes()).to_hex())
        .expect("a BLAKE3 digest is an address")
}

/// `case`'s account `name`.
fn user(case: &str, name: &str) -> UserId {
    UserId::new(format!("{case}-{name}"))
}

/// A whole-second instant `secs` after a fixed base, so equality is exact on every adapter.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// A link for `case` owned by `owner`, with `caps`.
fn link(case: &str, n: u8, owner: &UserId, caps: LinkCaps) -> UploadLinkRecord {
    UploadLinkRecord {
        opaque_id: opaque(case, n),
        owner_id: owner.clone(),
        drop_pubkey: vec![7; 32],
        crypto_suite_id: 1,
        protocol_version: "2026-06-01".to_owned(),
        caps,
        passphrase_verifier: None,
        used_bytes: 0,
        used_files: 0,
        revoked_at: None,
    }
}

/// An inbox entry for `owner`, received at `received`.
fn entry(case: &str, drop: &str, owner: &UserId, received: Timestamp) -> InboxEntry {
    InboxEntry {
        drop_id: format!("{case}-{drop}"),
        owner_id: owner.clone(),
        opaque_id: opaque(case, 1),
        address: address(&format!("{case}/{drop}")),
        size: 4096,
        content_type: "image/jpeg".to_owned(),
        kem_ct: vec![0, 1, 2, 0xff],
        suggested_filename: Some("IMG_0001.JPG".to_owned()),
        received_at: received,
        adopting: false,
    }
}

/// Provision `record`, expecting it to be recorded.
async fn provision(h: &dyn Harness, record: UploadLinkRecord) {
    assert_eq!(
        ok(h.drops().provision(record).await, "provision a link"),
        LinkProvisionOutcome::Provisioned,
        "a link under a fresh opaque id must be provisioned"
    );
}

/// Provision `record` under an id already held, expecting the refusal.
async fn provision_refused(h: &dyn Harness, record: UploadLinkRecord) {
    assert_eq!(
        ok(h.drops().provision(record).await, "provision a link"),
        LinkProvisionOutcome::Conflict,
        "provisioning under an opaque id already held must be refused"
    );
}

/// What `opaque_id` resolves to.
async fn resolve(h: &dyn Harness, opaque_id: &str) -> Option<UploadLinkRecord> {
    ok(h.drops().resolve(opaque_id).await, "resolve a link")
}

/// Charge `size` bytes against `opaque_id` at `when`.
async fn charge(h: &dyn Harness, opaque_id: &str, size: u64, when: Timestamp) -> Admission {
    ok(
        h.drops().charge(opaque_id, size, when).await,
        "charge a link",
    )
}

/// `(used_bytes, used_files)` on `opaque_id`.
async fn totals(h: &dyn Harness, opaque_id: &str) -> (u64, u32) {
    let link = resolve(h, opaque_id).await.expect("the link exists");
    (link.used_bytes, link.used_files)
}

/// `owner`'s inbox, as drop ids.
async fn inbox_ids(h: &dyn Harness, owner: &UserId) -> Vec<String> {
    ok(h.drops().inbox(owner).await, "list an inbox")
        .into_iter()
        .map(|entry| entry.drop_id)
        .collect()
}

// ===========================================================================================
// Links
// ===========================================================================================

/// An unknown id resolves to `None` and charges as `NotLive`.
pub async fn an_unknown_link_resolves_to_none_and_is_not_live(h: &dyn Harness) {
    let id = opaque("unknown", 0);
    assert_eq!(resolve(h, &id).await, None);
    assert_eq!(charge(h, &id, 1, at(0)).await, Admission::NotLive);
}

/// A provisioned link resolves to its whole record, every cap and optional field included.
pub async fn a_provisioned_link_resolves_to_its_whole_record(h: &dyn Harness) {
    let case = "whole";
    let mut record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            expires_at: Some(at(3_600)),
            max_total_bytes: Some(1 << 30),
            max_file_count: Some(10),
            max_file_size: Some(1 << 20),
            single_use: true,
        },
    );
    record.passphrase_verifier = Some(b"$argon2id$v=19$opaque".to_vec());
    record.crypto_suite_id = u16::MAX;
    provision(h, record.clone()).await;
    assert_eq!(resolve(h, &record.opaque_id).await, Some(record));

    let bare = link(case, 2, &user(case, "alice"), LinkCaps::default());
    provision(h, bare.clone()).await;
    assert_eq!(resolve(h, &bare.opaque_id).await, Some(bare));
}

/// Provisioning under an opaque id another account holds is refused, and the held link — owner,
/// key, caps and the totals already charged — reads back unchanged.
pub async fn provisioning_a_held_id_is_refused_and_the_link_is_unchanged(h: &dyn Harness) {
    let case = "reprovision";
    let first = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            expires_at: Some(at(3_600)),
            max_total_bytes: Some(1 << 30),
            max_file_count: Some(10),
            max_file_size: Some(1 << 20),
            single_use: true,
        },
    );
    provision(h, first.clone()).await;
    assert!(matches!(
        charge(h, &first.opaque_id, 4096, at(0)).await,
        Admission::Admitted { .. }
    ));

    let charged = resolve(h, &first.opaque_id).await.expect("provisioned");

    let mut second = link(case, 1, &user(case, "mallory"), LinkCaps::default());
    second.drop_pubkey = vec![9; 32];
    second.crypto_suite_id = 2;
    "2026-09-01".clone_into(&mut second.protocol_version);
    second.passphrase_verifier = Some(b"$argon2id$v=19$other".to_vec());
    second.used_bytes = 7;
    second.used_files = 3;
    second.revoked_at = Some(at(5));
    provision_refused(h, second).await;
    assert_eq!(resolve(h, &first.opaque_id).await, Some(charged));
}

/// The owner cannot re-provision its own id either: a re-provision carrying zero totals would
/// otherwise lift a spent cap, and one without `revoked_at` would un-revoke the link.
pub async fn the_owner_cannot_reprovision_a_spent_or_revoked_link(h: &dyn Harness) {
    let case = "respend";
    let alice = user(case, "alice");
    let record = link(
        case,
        1,
        &alice,
        LinkCaps {
            single_use: true,
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    assert!(matches!(
        charge(h, &record.opaque_id, 1, at(0)).await,
        Admission::Admitted { .. }
    ));
    provision_refused(h, record.clone()).await;
    assert_eq!(
        charge(h, &record.opaque_id, 1, at(1)).await,
        Admission::NotLive,
        "a refused re-provision must not reset a spent link's totals"
    );

    let other = link(case, 2, &alice, LinkCaps::default());
    provision(h, other.clone()).await;
    assert!(ok(
        h.drops().revoke(&alice, &other.opaque_id, at(2)).await,
        "revoke a link"
    ));
    provision_refused(h, other.clone()).await;
    assert_eq!(
        resolve(h, &other.opaque_id)
            .await
            .expect("provisioned")
            .revoked_at,
        Some(at(2)),
        "a refused re-provision must not un-revoke the link"
    );
}

/// An admission reserves one file and its bytes, and names the link's owner and pins.
pub async fn an_admission_reserves_one_file_and_its_bytes(h: &dyn Harness) {
    let case = "admit";
    let alice = user(case, "alice");
    let record = link(case, 1, &alice, LinkCaps::default());
    provision(h, record.clone()).await;
    assert_eq!(
        charge(h, &record.opaque_id, 100, at(0)).await,
        Admission::Admitted {
            owner_id: alice,
            crypto_suite_id: record.crypto_suite_id,
            protocol_version: record.protocol_version.clone(),
        }
    );
    charge(h, &record.opaque_id, 50, at(1)).await;
    assert_eq!(totals(h, &record.opaque_id).await, (150, 2));
}

/// A file past the per-file limit is `FileTooLarge` — before the cumulative caps — and
/// reserves nothing.
pub async fn a_file_past_the_size_limit_is_refused_first_and_reserves_nothing(h: &dyn Harness) {
    let case = "too-large";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            max_file_size: Some(1_000),
            max_file_count: Some(1),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    charge(h, &record.opaque_id, 1_000, at(0)).await;
    // The count cap is now exhausted too; the size refusal is still the answer.
    assert_eq!(
        charge(h, &record.opaque_id, 1_001, at(1)).await,
        Admission::FileTooLarge { limit: 1_000 }
    );
    assert_eq!(totals(h, &record.opaque_id).await, (1_000, 1));
}

/// The file-count cap admits up to its count and refuses the next, which reserves nothing.
pub async fn the_file_count_cap_is_never_exceeded(h: &dyn Harness) {
    let case = "count";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            max_file_count: Some(2),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    for second in 0..2 {
        assert!(matches!(
            charge(h, &record.opaque_id, 10, at(second)).await,
            Admission::Admitted { .. }
        ));
    }
    assert_eq!(
        charge(h, &record.opaque_id, 10, at(2)).await,
        Admission::CapExhausted
    );
    assert_eq!(totals(h, &record.opaque_id).await, (20, 2));
}

/// The byte cap admits an exact fit and refuses one byte more, which reserves nothing.
pub async fn the_byte_cap_admits_an_exact_fit_and_nothing_more(h: &dyn Harness) {
    let case = "bytes";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            max_total_bytes: Some(1_000),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    assert!(matches!(
        charge(h, &record.opaque_id, 600, at(0)).await,
        Admission::Admitted { .. }
    ));
    assert_eq!(
        charge(h, &record.opaque_id, 401, at(1)).await,
        Admission::CapExhausted
    );
    assert_eq!(totals(h, &record.opaque_id).await, (600, 1));
    assert!(matches!(
        charge(h, &record.opaque_id, 400, at(2)).await,
        Admission::Admitted { .. }
    ));
    assert_eq!(totals(h, &record.opaque_id).await, (1_000, 2));
}

/// A single-use link is spent by its first admission.
pub async fn a_single_use_link_is_spent_by_its_first_drop(h: &dyn Harness) {
    let case = "single";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            single_use: true,
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    assert!(matches!(
        charge(h, &record.opaque_id, 10, at(0)).await,
        Admission::Admitted { .. }
    ));
    assert_eq!(
        charge(h, &record.opaque_id, 10, at(1)).await,
        Admission::NotLive
    );
    assert_eq!(totals(h, &record.opaque_id).await, (10, 1));
}

/// A link past its expiry is `NotLive` from that instant on, and reserves nothing.
pub async fn an_expired_link_is_not_live(h: &dyn Harness) {
    let case = "expired";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            expires_at: Some(at(100)),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    assert!(matches!(
        charge(h, &record.opaque_id, 10, at(99)).await,
        Admission::Admitted { .. }
    ));
    assert_eq!(
        charge(h, &record.opaque_id, 10, at(100)).await,
        Admission::NotLive
    );
    assert_eq!(totals(h, &record.opaque_id).await, (10, 1));
}

/// The owner revokes once; a stranger, a second revocation and an unknown id change nothing; a
/// revoked link is `NotLive`.
pub async fn revocation_is_the_owners_once_and_ends_admission(h: &dyn Harness) {
    let case = "revoke";
    let alice = user(case, "alice");
    let record = link(case, 1, &alice, LinkCaps::default());
    provision(h, record.clone()).await;

    let revoke = |owner: UserId, id: String, when: Timestamp| async move {
        ok(h.drops().revoke(&owner, &id, when).await, "revoke a link")
    };
    assert!(!revoke(user(case, "mallory"), record.opaque_id.clone(), at(5)).await);
    assert_eq!(resolve(h, &record.opaque_id).await, Some(record.clone()));
    assert!(revoke(alice.clone(), record.opaque_id.clone(), at(10)).await);
    assert!(!revoke(alice.clone(), record.opaque_id.clone(), at(20)).await);
    assert!(!revoke(alice, opaque(case, 9), at(20)).await);

    let held = resolve(h, &record.opaque_id).await.expect("still resolves");
    assert_eq!(held.revoked_at, Some(at(10)));
    assert_eq!(
        charge(h, &record.opaque_id, 1, at(11)).await,
        Admission::NotLive
    );
    assert_eq!(totals(h, &record.opaque_id).await, (0, 0));
}

/// A refund gives back one file and its bytes, floors at zero, and ignores an unknown link.
pub async fn a_refund_gives_back_a_reservation_and_floors_at_zero(h: &dyn Harness) {
    let case = "refund";
    let record = link(
        case,
        1,
        &user(case, "alice"),
        LinkCaps {
            max_file_count: Some(1),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    charge(h, &record.opaque_id, 300, at(0)).await;
    ok(h.drops().refund(&record.opaque_id, 300).await, "refund");
    assert_eq!(totals(h, &record.opaque_id).await, (0, 0));
    // The slot is back: a link capped at one file admits again.
    assert!(matches!(
        charge(h, &record.opaque_id, 300, at(1)).await,
        Admission::Admitted { .. }
    ));

    ok(h.drops().refund(&record.opaque_id, 10_000).await, "refund");
    ok(h.drops().refund(&record.opaque_id, 10_000).await, "refund");
    assert_eq!(totals(h, &record.opaque_id).await, (0, 0));

    ok(
        h.drops().refund(&opaque(case, 9), 1).await,
        "refund an unknown link",
    );
    assert_eq!(resolve(h, &opaque(case, 9)).await, None);
}

// ===========================================================================================
// Declarations
// ===========================================================================================

/// A declaration is held for its session and taken back exactly once.
pub async fn a_declaration_is_taken_back_exactly_once(h: &dyn Harness) {
    let case = "reserve";
    let upload = UploadId::new(format!("{case}-upload"));
    let pending = PendingDeposit {
        drop_id: format!("{case}-drop"),
        opaque_id: opaque(case, 1),
        owner_id: user(case, "alice"),
        kem_ct: vec![0xff, 0, 1],
        content_type: "video/mp4".to_owned(),
        suggested_filename: None,
        size: 5_000,
    };
    ok(h.drops().reserve(pending.clone(), &upload).await, "reserve");
    assert_eq!(
        ok(h.drops().take_reservation(&upload).await, "take"),
        Some(pending)
    );
    assert_eq!(ok(h.drops().take_reservation(&upload).await, "take"), None);
    assert_eq!(
        ok(
            h.drops()
                .take_reservation(&UploadId::new(format!("{case}-other")))
                .await,
            "take an unknown session's"
        ),
        None
    );
}

/// A second declaration for the same session replaces the first.
pub async fn a_second_declaration_for_a_session_replaces_the_first(h: &dyn Harness) {
    let case = "redeclare";
    let upload = UploadId::new(format!("{case}-upload"));
    let first = PendingDeposit {
        drop_id: format!("{case}-first"),
        opaque_id: opaque(case, 1),
        owner_id: user(case, "alice"),
        kem_ct: vec![1],
        content_type: "image/png".to_owned(),
        suggested_filename: Some("a.png".to_owned()),
        size: 1,
    };
    let second = PendingDeposit {
        drop_id: format!("{case}-second"),
        size: 2,
        ..first.clone()
    };
    ok(h.drops().reserve(first, &upload).await, "reserve");
    ok(h.drops().reserve(second.clone(), &upload).await, "reserve");
    assert_eq!(
        ok(h.drops().take_reservation(&upload).await, "take"),
        Some(second)
    );
}

// ===========================================================================================
// The inbox and adoption
// ===========================================================================================

/// The inbox is the owner's, oldest first, ties broken by id, whole entries.
pub async fn the_inbox_is_the_owners_oldest_first(h: &dyn Harness) {
    let case = "inbox";
    let alice = user(case, "alice");
    let bob = user(case, "bob");
    let late = entry(case, "a-late", &alice, at(20));
    let tie_b = entry(case, "b-tie", &alice, at(10));
    let tie_a = entry(case, "a-tie", &alice, at(10));
    let bobs = entry(case, "bobs", &bob, at(0));
    for entry in [late.clone(), tie_b.clone(), tie_a.clone(), bobs] {
        ok(h.drops().deposit(entry).await, "deposit");
    }
    assert_eq!(
        ok(h.drops().inbox(&alice).await, "list an inbox"),
        vec![tie_a, tie_b, late]
    );
    assert_eq!(
        inbox_ids(h, &user(case, "carol")).await,
        Vec::<String>::new()
    );
}

/// A claim holds the row once: the owner gets it, a second claim and a stranger's get `None`.
pub async fn a_claim_holds_the_row_once(h: &dyn Harness) {
    let case = "claim";
    let alice = user(case, "alice");
    let landed = entry(case, "drop", &alice, at(0));
    ok(h.drops().deposit(landed.clone()).await, "deposit");

    assert_eq!(
        ok(
            h.drops()
                .claim(&user(case, "mallory"), &landed.drop_id)
                .await,
            "claim as a stranger"
        ),
        None
    );
    let claimed = ok(h.drops().claim(&alice, &landed.drop_id).await, "claim")
        .expect("the owner claims an unheld row");
    assert_eq!(
        claimed,
        InboxEntry {
            adopting: true,
            ..landed.clone()
        }
    );
    assert_eq!(
        ok(
            h.drops().claim(&alice, &landed.drop_id).await,
            "claim again"
        ),
        None,
        "a held row is not handed out twice"
    );
    assert!(
        ok(h.drops().inbox(&alice).await, "list")[0].adopting,
        "the hold is visible in the inbox"
    );
    assert_eq!(
        ok(
            h.drops().claim(&alice, &format!("{case}-none")).await,
            "claim"
        ),
        None
    );
}

/// Release returns a claimed row, which can then be claimed again.
pub async fn a_released_row_can_be_claimed_again(h: &dyn Harness) {
    let case = "release";
    let alice = user(case, "alice");
    let landed = entry(case, "drop", &alice, at(0));
    ok(h.drops().deposit(landed.clone()).await, "deposit");
    ok(h.drops().claim(&alice, &landed.drop_id).await, "claim").expect("claimed");
    ok(h.drops().release(&landed.drop_id).await, "release");
    assert_eq!(
        ok(h.drops().inbox(&alice).await, "list"),
        vec![landed.clone()]
    );
    assert!(ok(h.drops().claim(&alice, &landed.drop_id).await, "claim").is_some());
    ok(
        h.drops().release(&format!("{case}-none")).await,
        "release an unknown row",
    );
}

/// Settle removes a claimed row, and the owner's other drops stay.
pub async fn a_settled_row_leaves_the_inbox(h: &dyn Harness) {
    let case = "settle";
    let alice = user(case, "alice");
    let adopted = entry(case, "adopted", &alice, at(0));
    let waiting = entry(case, "waiting", &alice, at(1));
    ok(h.drops().deposit(adopted.clone()).await, "deposit");
    ok(h.drops().deposit(waiting.clone()).await, "deposit");
    ok(h.drops().claim(&alice, &adopted.drop_id).await, "claim").expect("claimed");
    ok(h.drops().settle(&adopted.drop_id).await, "settle");
    assert_eq!(inbox_ids(h, &alice).await, vec![waiting.drop_id]);
}

/// Discard is the owner's: `true` once, `false` for a stranger and an unknown id.
pub async fn a_discard_is_the_owners(h: &dyn Harness) {
    let case = "discard";
    let alice = user(case, "alice");
    let landed = entry(case, "drop", &alice, at(0));
    ok(h.drops().deposit(landed.clone()).await, "deposit");
    assert!(!ok(
        h.drops()
            .discard(&user(case, "mallory"), &landed.drop_id)
            .await,
        "discard as a stranger"
    ));
    assert_eq!(inbox_ids(h, &alice).await, vec![landed.drop_id.clone()]);
    assert!(ok(
        h.drops().discard(&alice, &landed.drop_id).await,
        "discard"
    ));
    assert!(!ok(
        h.drops().discard(&alice, &landed.drop_id).await,
        "discard"
    ));
    assert!(inbox_ids(h, &alice).await.is_empty());
}

/// Depositing under a drop id already held replaces the whole entry, its hold included: the
/// earlier owner's inbox no longer shows it, and the new owner sees the new entry, unheld.
pub async fn redepositing_a_held_drop_id_replaces_the_entry(h: &dyn Harness) {
    let case = "redeposit";
    let alice = user(case, "alice");
    let bob = user(case, "bob");
    let first = entry(case, "drop", &alice, at(0));
    ok(h.drops().deposit(first.clone()).await, "deposit");
    ok(h.drops().claim(&alice, &first.drop_id).await, "claim").expect("claimed");

    let second = InboxEntry {
        drop_id: first.drop_id.clone(),
        opaque_id: opaque(case, 2),
        address: address(&format!("{case}/again")),
        size: 1,
        content_type: "image/png".to_owned(),
        kem_ct: vec![9, 9],
        suggested_filename: None,
        ..entry(case, "drop", &bob, at(7))
    };
    ok(h.drops().deposit(second.clone()).await, "deposit again");
    assert!(inbox_ids(h, &alice).await.is_empty());
    assert_eq!(ok(h.drops().inbox(&bob).await, "list"), vec![second]);
}

/// Instants lose at most sub-microsecond precision and are never moved later.
pub async fn instants_read_back_at_microsecond_precision(h: &dyn Harness) {
    let case = "precise";
    let alice = user(case, "alice");
    let expiry = Timestamp::from_nanosecond(1_700_003_600_123_456_789).expect("an instant");
    let record = link(
        case,
        1,
        &alice,
        LinkCaps {
            expires_at: Some(expiry),
            ..LinkCaps::default()
        },
    );
    provision(h, record.clone()).await;
    let revoked = Timestamp::from_nanosecond(1_700_000_010_987_654_321).expect("an instant");
    assert!(ok(
        h.drops().revoke(&alice, &record.opaque_id, revoked).await,
        "revoke"
    ));
    let received = Timestamp::from_nanosecond(1_700_000_005_555_555_555).expect("an instant");
    ok(
        h.drops()
            .deposit(entry(case, "drop", &alice, received))
            .await,
        "deposit",
    );

    let held = resolve(h, &record.opaque_id).await.expect("provisioned");
    let landed = ok(h.drops().inbox(&alice).await, "list")[0].received_at;
    for (written, read) in [
        (expiry, held.caps.expires_at.expect("present")),
        (revoked, held.revoked_at.expect("present")),
        (received, landed),
    ] {
        assert!(read <= written, "{read} is later than {written}");
        assert!(
            written.duration_since(read) < SignedDuration::from_micros(1),
            "{read} lost more than sub-microsecond precision from {written}"
        );
    }
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    an_unknown_link_resolves_to_none_and_is_not_live(h).await;
    a_provisioned_link_resolves_to_its_whole_record(h).await;
    provisioning_a_held_id_is_refused_and_the_link_is_unchanged(h).await;
    the_owner_cannot_reprovision_a_spent_or_revoked_link(h).await;
    an_admission_reserves_one_file_and_its_bytes(h).await;
    a_file_past_the_size_limit_is_refused_first_and_reserves_nothing(h).await;
    the_file_count_cap_is_never_exceeded(h).await;
    the_byte_cap_admits_an_exact_fit_and_nothing_more(h).await;
    a_single_use_link_is_spent_by_its_first_drop(h).await;
    an_expired_link_is_not_live(h).await;
    revocation_is_the_owners_once_and_ends_admission(h).await;
    a_refund_gives_back_a_reservation_and_floors_at_zero(h).await;

    a_declaration_is_taken_back_exactly_once(h).await;
    a_second_declaration_for_a_session_replaces_the_first(h).await;

    the_inbox_is_the_owners_oldest_first(h).await;
    a_claim_holds_the_row_once(h).await;
    a_released_row_can_be_claimed_again(h).await;
    a_settled_row_leaves_the_inbox(h).await;
    a_discard_is_the_owners(h).await;
    redepositing_a_held_drop_id_replaces_the_entry(h).await;
    instants_read_back_at_microsecond_precision(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::drop::{DropStore, InMemoryDrops};

    /// The deterministic store.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        drops: InMemoryDrops,
    }

    impl Harness for MemoryHarness {
        fn drops(&self) -> &dyn DropStore {
            &self.drops
        }
    }

    /// Declares one `#[tokio::test]` per conformance case, on a fresh store each.
    macro_rules! conformance_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(&MemoryHarness::default()).await;
                }
            )+
        };
    }

    conformance_cases! {
        an_unknown_link_resolves_to_none_and_is_not_live,
        a_provisioned_link_resolves_to_its_whole_record,
        provisioning_a_held_id_is_refused_and_the_link_is_unchanged,
        the_owner_cannot_reprovision_a_spent_or_revoked_link,
        an_admission_reserves_one_file_and_its_bytes,
        a_file_past_the_size_limit_is_refused_first_and_reserves_nothing,
        the_file_count_cap_is_never_exceeded,
        the_byte_cap_admits_an_exact_fit_and_nothing_more,
        a_single_use_link_is_spent_by_its_first_drop,
        an_expired_link_is_not_live,
        revocation_is_the_owners_once_and_ends_admission,
        a_refund_gives_back_a_reservation_and_floors_at_zero,
        a_declaration_is_taken_back_exactly_once,
        a_second_declaration_for_a_session_replaces_the_first,
        the_inbox_is_the_owners_oldest_first,
        a_claim_holds_the_row_once,
        a_released_row_can_be_claimed_again,
        a_settled_row_leaves_the_inbox,
        a_discard_is_the_owners,
        redepositing_a_held_drop_id_replaces_the_entry,
        instants_read_back_at_microsecond_precision,
    }

    /// The whole suite, in one pass on one store — the entry point the container case uses.
    #[tokio::test]
    async fn the_in_memory_store_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
