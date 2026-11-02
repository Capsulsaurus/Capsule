use super::*;

#[test]
fn superseded_heads_round_trip_through_their_column() {
    let heads = vec![Hash32([1; 32]), Hash32([2; 32]), Hash32([3; 32])];
    let column = encode_heads(&heads);
    assert_eq!(column.len(), 96);
    assert_eq!(decode_heads(&column).expect("decodes"), heads);
    assert_eq!(decode_heads(&[]).expect("decodes"), Vec::<Hash32>::new());
}

#[test]
fn a_superseded_column_of_a_partial_hash_is_corrupt() {
    let error = decode_heads(&[0; 33]).expect_err("a partial hash");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    let error = hash_from_bytes(&[0; 31]).expect_err("a short head");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

#[test]
fn changes_and_roles_read_back_and_nothing_else_does() {
    for change in [Change::Created, Change::Updated, Change::Deleted] {
        assert_eq!(change_from(change_name(change)).expect("a change"), change);
    }
    assert!(matches!(
        change_from("moved"),
        Err(StoreError::Corrupt { .. })
    ));
    for role in ["original", "metadata", "derivative", "provenance"] {
        assert_eq!(role_from(role).expect("a role"), role);
    }
    assert!(matches!(
        role_from("thumb"),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn counts_past_a_bigint_are_refused_and_negative_columns_are_corrupt() {
    assert_eq!(to_column(7).expect("fits"), 7);
    assert!(matches!(
        to_column(u64::MAX),
        Err(StoreError::Rejected { .. })
    ));
    assert!(matches!(
        count_from::<u64>(-1, "count"),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        count_from::<u32>(i64::from(u32::MAX) + 1, "epoch"),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn a_hash_is_its_own_content_address() {
    let hash = Hash32([0xab; 32]);
    assert_eq!(
        address_of(&hash).expect("an address").as_str(),
        hash.to_hex()
    );
    assert_eq!(hash_from_hex(&hash.to_hex()).expect("a hash"), hash);
    assert!(matches!(
        hash_from_hex("not hex"),
        Err(StoreError::Corrupt { .. })
    ));
}

/// The suite and the charging hook, against a real Postgres.
mod postgres_conformance {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use capsule_core::crypto::hash::hash_bytes;
    use sea_orm::DatabaseTransaction;

    use super::super::{HeldBlob, PeerBudgetCharge, PostgresPulledAlbums, PulledBlobCharge};
    use crate::blob::{BlobStore, InMemoryBlobStore};
    use crate::federation::PeerId;
    use crate::federation::pull::JobKey;
    use crate::federation::pull::pulled::conformance::{self, Inspect as _};
    use crate::federation::pull::pulled::{
        Committed, OverBudget, PeerCachingBudget, PulledAlbum, PulledAlbums as _, ValidatedPage,
    };
    use crate::federation::subscriptions::postgres::PostgresSubscriptions;
    use crate::postgres::testing;
    use crate::store::{AlbumId, StoreFuture, UserId};

    /// Records every charge, and refuses them all once told to.
    #[derive(Debug, Default)]
    struct Recording {
        charged: Mutex<Vec<(JobKey, Vec<HeldBlob>)>>,
        refuse: Mutex<bool>,
    }

    impl Recording {
        fn charged(&self) -> Vec<(JobKey, Vec<HeldBlob>)> {
            self.charged
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn refuse(&self) {
            *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) = true;
        }
    }

    impl PulledBlobCharge for Recording {
        fn charge<'a>(
            &'a self,
            _transaction: &'a DatabaseTransaction,
            job: &'a JobKey,
            held: &'a [HeldBlob],
        ) -> StoreFuture<'a, Option<OverBudget>> {
            Box::pin(async move {
                if *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) {
                    return Ok(Some(refusal(job)));
                }
                self.charged
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((job.clone(), held.to_vec()));
                Ok(None)
            })
        }
    }

    /// What [`Recording`] refuses with.
    fn refusal(job: &JobKey) -> OverBudget {
        OverBudget {
            subscriber: UserId::new("someone"),
            peer: job.peer.clone(),
            used: 2,
            budget: 1,
        }
    }

    fn page(high_water: u64, blobs: &[&[u8]]) -> ValidatedPage {
        ValidatedPage {
            entries: Vec::new(),
            metadata_blobs: blobs
                .iter()
                .map(|bytes| (hash_bytes(bytes), bytes.to_vec()))
                .collect::<BTreeMap<_, _>>(),
            state: PulledAlbum {
                cursor: Some(format!("c{high_water}")),
                high_water: Some(high_water),
                ..PulledAlbum::default()
            },
        }
    }

    /// Only the blobs a job holds for the first time are charged, and a refusal writes nothing.
    async fn charging(store: &PostgresPulledAlbums, blobs: &InMemoryBlobStore, hook: &Recording) {
        let job = JobKey::new(PeerId::new("home.charge.test"), AlbumId::new("charge-a"));
        let first: &[u8] = b"the first metadata blob";
        let second: &[u8] = b"the second metadata blob";
        for (from, next) in [
            (None, page(1, &[first])),
            (Some(1), page(2, &[first, second])),
            (Some(2), page(3, &[])),
        ] {
            assert_eq!(
                store.commit(&job, from, next).await.expect("commit"),
                Committed::Written
            );
        }
        assert_eq!(
            hook.charged(),
            vec![
                (
                    job.clone(),
                    vec![HeldBlob {
                        hash: hash_bytes(first),
                        size: 23,
                    }]
                ),
                (
                    job.clone(),
                    vec![HeldBlob {
                        hash: hash_bytes(second),
                        size: 24,
                    }]
                ),
            ],
            "a blob is charged once per job, and a page holding nothing new charges nothing"
        );

        hook.refuse();
        let third: &[u8] = b"a blob over the cap";
        let mut refused = page(4, &[third]);
        refused.state.assets.insert(
            "asset".to_owned(),
            crate::federation::pull::pulled::AssetHead {
                head: hash_bytes(b"head"),
                superseded: Vec::new(),
                deleted: false,
                amk_version: 1,
                metadata_blob: Some(hash_bytes(third)),
            },
        );
        assert_eq!(
            store
                .commit(&job, Some(3), refused)
                .await
                .expect("the charge refuses"),
            Committed::OverBudget(refusal(&job)),
            "the hook's refusal is the commit's answer"
        );
        let state = store.read(&job).await.expect("read");
        assert_eq!(state.high_water, Some(3), "the refused page moved nothing");
        assert!(state.assets.is_empty(), "the refused page wrote no head");
        assert_eq!(
            store
                .held_metadata_blob(&job, &hash_bytes(third))
                .await
                .expect("lookup"),
            None,
            "the refused page's blob is not held by the job"
        );
        // Its bytes reached the blob store before the refusal: an unreferenced orphan, which the
        // collector reclaims, and never a row naming bytes that are not there.
        let address =
            crate::blob::ContentAddress::parse(&hash_bytes(third).to_hex()).expect("an address");
        assert!(blobs.stat(&address).await.expect("stat").is_some());
    }

    #[tokio::test]
    async fn the_postgres_pulled_album_store_conforms() {
        let Some(database) = testing::start("the Postgres pulled-album store").await else {
            return;
        };
        let blobs = Arc::new(InMemoryBlobStore::new());
        let store = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone());
        conformance::run_all(&store).await;

        let hook = Arc::new(Recording::default());
        let charged = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone())
            .with_charge(hook.clone());
        charging(&charged, &blobs, &hook).await;

        let subscriptions = PostgresSubscriptions::new(database.connection().clone());
        let budgeted = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone())
            .with_charge(Arc::new(PeerBudgetCharge::new(PeerCachingBudget::bytes(
                conformance::BUDGET,
            ))));
        conformance::run_budget(&budgeted, &subscriptions).await;
    }
}
