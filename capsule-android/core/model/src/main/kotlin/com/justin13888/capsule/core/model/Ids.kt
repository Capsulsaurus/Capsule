package com.justin13888.capsule.core.model

/*
 * Identifier value classes.
 *
 * Every id in the catalog is a UUID string on the wire (`AssetRecord.uuid`,
 * `AlbumRecord.id`, `AssetStackRecord.id`, …), so nothing but the declared type stops an
 * album id being passed where an asset id belongs — a mix-up the compiler would otherwise
 * wave through and that would surface as a silent empty query. Wrapping each in an inline
 * value class makes those mistakes compile errors while costing nothing at runtime: the
 * JVM still sees a bare `String` at every call site.
 *
 * None of these validate their contents. Ids arrive from the catalog already minted by
 * `capsule-core`, and a client that re-checks them would only be guessing at a rule it does
 * not own — see [Metadata — Identifiers](https://docs/design/metadata/#identifiers) for who
 * mints what and which UUID version each carries.
 */

/**
 * A single imported media file — `AssetRecord.uuid`.
 *
 * One canonical identity per asset: the sidecar `uuid`, the manifest `file_id`, the
 * provenance chain's `asset_id` and the metadata blob's `blob_id` are all this same UUIDv7,
 * minted once at import and never re-minted.
 */
@JvmInline
value class AssetId(
    val value: String,
)

/** A container or view album — `AlbumRecord.id`. */
@JvmInline
value class AlbumId(
    val value: String,
)

/** A group of related assets — `AssetStackRecord.id`, a UUIDv7. */
@JvmInline
value class StackId(
    val value: String,
)

/** One asset's membership row inside a stack — `StackMemberRecord.id`. */
@JvmInline
value class StackMemberId(
    val value: String,
)

/**
 * An enrolled device — UUIDv4, deliberately *not* v7.
 *
 * A device id must not leak its creation ordering, so it is the one identifier in the
 * system that is unordered by construction.
 */
@JvmInline
value class DeviceId(
    val value: String,
)

/** The authenticated session an asset was imported in — UUIDv7. */
@JvmInline
value class SessionId(
    val value: String,
)

/** An account. Quota is accounted to the uploader, which may not be the owner. */
@JvmInline
value class UserId(
    val value: String,
)

/** A user-defined smart album's definition — UUIDv7, the key into `smart_albums`. */
@JvmInline
value class SmartAlbumId(
    val value: String,
)

/** One import run, spanning every file the planner accepted — UUIDv7. */
@JvmInline
value class ImportId(
    val value: String,
)

/**
 * The canonical identity of an import source — `SHA-256` over domain-separated canonical
 * CBOR of `(platform, source_kind, locator)`, so two devices looking at the same source
 * compute the same scope with no coordination protocol.
 */
@JvmInline
value class ScopeId(
    val value: String,
)

/** A named person the user has confirmed, distinct from the raw cluster behind them. */
@JvmInline
value class PersonId(
    val value: String,
)

/** An unnamed grouping of face detections produced by a single model version. */
@JvmInline
value class FaceClusterId(
    val value: String,
)

/** A share link issued to a non-member of the album. */
@JvmInline
value class ShareLinkId(
    val value: String,
)

/** A web-upload link that lets a non-account recipient deposit files into an inbox. */
@JvmInline
value class UploadLinkId(
    val value: String,
)

/** One deposited-but-not-yet-adopted file sitting in a drop inbox. */
@JvmInline
value class DropId(
    val value: String,
)

/** A resumable upload session — one per asset per [UploadTier]. */
@JvmInline
value class UploadSessionId(
    val value: String,
)

/** A quarantined inbound artifact awaiting the user's decision. */
@JvmInline
value class QuarantineId(
    val value: String,
)

/** One exported encrypted backup artifact. */
@JvmInline
value class BackupArtifactId(
    val value: String,
)

/** A moderation report or action, as surfaced to the reporting user. */
@JvmInline
value class ModerationEventId(
    val value: String,
)

/**
 * The content address of a blob — the hash of its **ciphertext**, lowercase hex.
 *
 * Blobs are content-addressed by ciphertext hash, so a fetch is skipped entirely when the
 * hash is already in the local cache; that is what makes a thumbnail shared between two
 * assets cost one download rather than two.
 */
@JvmInline
value class BlobHash(
    val value: String,
)

/**
 * The canonical plaintext digest of an asset — `AssetRecord.hash_sha256`, lowercase hex.
 *
 * Distinct from [BlobHash]: this one is computed before encryption and is what
 * `Catalog.find_by_hash` matches on for import deduplication.
 */
@JvmInline
value class ContentHash(
    val value: String,
)

/** The opaque, server-MAC'd position in the `/sync` feed. Never parsed by a client. */
@JvmInline
value class SyncCursor(
    val value: String,
)

/** A stable model identifier such as `mobileclip-b`, constant across versions. */
@JvmInline
value class ModelId(
    val value: String,
)

/** A model version, bumped on every swap for a task; prior outputs then read as stale. */
@JvmInline
value class ModelVersion(
    val value: String,
)
