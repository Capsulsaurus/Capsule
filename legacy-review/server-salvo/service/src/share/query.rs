use data_encoding::BASE64;
use entity::public_share;
use jiff::Timestamp;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use super::{ServeAsset, ServeRecord, ShareResolution, StoredMetadata};

pub struct Query;

impl Query {
    /// Resolve an opaque id to a serve outcome (the single authoritative read the fail-closed
    /// serve cache backs). Order matters:
    ///
    /// 1. No row → [`ShareResolution::Gone`] (indistinguishable `404`).
    /// 2. `home_server != self_server` → [`ShareResolution::Foreign`] (the `{ home_server }`
    ///    pointer; a peer never serves content — Security Contract, Home-server-only serving).
    /// 3. Revoked or expired → [`ShareResolution::Gone`] (same indistinguishable `404`).
    /// 4. Otherwise → [`ShareResolution::Serve`] with the servable record.
    #[tracing::instrument(skip(db))]
    pub async fn resolve_by_opaque<C: ConnectionTrait>(
        db: &C,
        opaque_id: &str,
        self_server: &str,
        now: Timestamp,
    ) -> Result<ShareResolution, DbErr> {
        let Some(link) = public_share::Entity::find()
            .filter(public_share::Column::OpaqueId.eq(opaque_id))
            .one(db)
            .await?
        else {
            return Ok(ShareResolution::Gone);
        };

        // Home-server-only: a peer refuses to serve and returns the pointer, never content.
        if link.home_server != self_server {
            return Ok(ShareResolution::Foreign {
                home_server: link.home_server,
            });
        }

        // Fail-closed liveness: a revoked or expired link is an indistinguishable `404`.
        if !Self::is_live(&link, now) {
            return Ok(ShareResolution::Gone);
        }

        let assets = decode_assets(&link.served_metadata);
        Ok(ShareResolution::Serve(ServeRecord {
            opaque_id: link.opaque_id,
            scope_kind: link.scope_kind,
            scope_id: link.scope_id,
            home_server: link.home_server,
            wrapped_scope_b64: link.wrapped_scope,
            passphrase_protected: link.passphrase_protected,
            expires_at: link
                .expires_at
                .map(|e| entity::time::entity_to_ts(e).to_string()),
            assets,
        }))
    }

    /// Whether a fetched share row is live at `now` (not revoked, not expired).
    pub(super) fn is_live(link: &public_share::Model, now: Timestamp) -> bool {
        if link.revoked_at.is_some() {
            return false;
        }
        match link.expires_at {
            Some(exp) => entity::time::entity_to_ts(exp) > now,
            None => true,
        }
    }
}

/// Decode the stored `served_metadata` document into servable assets. A malformed document (only
/// possible from corruption, never a live publish path) yields an empty asset list rather than a
/// serve error — the link still resolves, just with no renderable assets.
fn decode_assets(served_metadata: &serde_json::Value) -> Vec<ServeAsset> {
    let parsed: StoredMetadata = match serde_json::from_value(served_metadata.clone()) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("share served_metadata decode failed: {e}");
            return Vec::new();
        }
    };
    parsed
        .assets
        .into_iter()
        .filter_map(|a| {
            let sidecar_cbor = BASE64.decode(a.sidecar_cbor_b64.as_bytes()).ok()?;
            Some(ServeAsset {
                asset_id: a.asset_id,
                content_hash: a.content_hash,
                content_type: a.content_type,
                size: a.size,
                sidecar_cbor,
                nonce_prefix_hex: a.nonce_prefix_hex,
                amk_version: a.amk_version,
            })
        })
        .collect()
}
