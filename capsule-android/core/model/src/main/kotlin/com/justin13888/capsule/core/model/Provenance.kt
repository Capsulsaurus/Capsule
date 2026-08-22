package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * A lifecycle operation on an asset — the closed action set.
 *
 * Note the wire tokens are **kebab-case**, unlike every other closed enum here; they are
 * spelled the way the signed manifest spells them, and a manifest's bytes are what the
 * signatures cover, so the spelling is not ours to normalise.
 *
 * A value outside this set is a structural error. Adding one requires a new, later-dated
 * `protocol_version`, and old albums stay pinned to their original set — which is precisely
 * what stops a new or faulty client injecting an unknown action into an older album.
 */
enum class LifecycleAction(
    override val wire: String,
) : WireEnum {
    /** First write of an asset. The only action with no prior chain link. */
    Create("create"),

    /** Replace the original bytes — same asset, new ciphertext and manifest. */
    Replace("replace"),

    /** Soft delete. The asset enters trash with its signed retention window. */
    Delete("delete"),

    /** An edit to the encrypted metadata blob or sidecar fields. */
    MetadataUpdate("metadata-update"),

    /** Add a thumbnail, preview, or embedding. */
    DerivativeAdd("derivative-add"),

    /** Replace an existing derivative — the only authorised path; a silent overwrite is refused. */
    DerivativeReplace("derivative-replace"),

    /** Recover a soft-deleted asset within its retention window. */
    TrashRestore("trash-restore"),
    ;

    /** Whether this action links to a prior chain head. Only [Create] does not. */
    val hasPriorLink: Boolean get() = this != Create

    companion object : WireCodec<LifecycleAction>({ entries })
}

/**
 * One link in an asset's append-only provenance chain.
 *
 * [priorProvenanceHash] is what makes a stale-revival attack detectable: a peer replaying an
 * old-but-validly-signed manifest names a chain head the receiver has already moved past, so
 * the entry is quarantined rather than applied. It is `null` exactly on
 * [LifecycleAction.Create].
 *
 * The chain is append-only in both directions — a delete record is never removed when the
 * asset is restored, so "deleted on X, restored on Y" stays legible forever.
 */
data class ProvenanceRecord(
    /** The asset this record belongs to. */
    val assetId: AssetId,
    /** What happened. */
    val action: Wire<LifecycleAction>,
    /** The chain head this write followed; absent only on `create`. */
    val priorProvenanceHash: String?,
    /** This record's own hash, which the next write will name. */
    val recordHash: String,
    /** The device that signed it. */
    val byDevice: DeviceId,
    /** When it was signed. */
    val at: Instant,
    /**
     * The signed floor on a purge, present on a `delete`.
     *
     * It lives in the manifest's server-visible envelope so a keyless purge worker can read
     * and enforce it without any decryption key — which is exactly what stops a hostile
     * server accelerating a purge by editing its own configuration.
     */
    val retentionUntil: Instant? = null,
)

/**
 * A code path that refuses to apply inbound state and surfaces it instead.
 *
 * The set exists so the UI and an operator audit share one inventory of "things that need a
 * human to look at". A quarantined item is **never silently dropped and never silently
 * applied**; the user can inspect, repair, or discard it explicitly.
 *
 * The wire tokens here are minted by this client — the design names these surfaces in prose
 * and does not assign them canonical strings — so they are a local vocabulary, not a
 * cross-implementation contract. They are treated as a closed set anyway so a surface cannot
 * be added without deciding how it is presented.
 */
enum class QuarantineSurface(
    override val wire: String,
) : WireEnum {
    /** Asset verification rejected a signature or a chain link. */
    VerifyAssetReject("verify_asset_reject"),

    /** A federated peer's artifact soft-failed; its hash is held in a bounded reject table. */
    FederationSoftFail("federation_soft_fail"),

    /** An original with no sidecar beside it, found during repair. */
    OrphanedOriginal("orphaned_original"),

    /** A sidecar whose CBOR would not parse. The unreadable bytes are preserved as-is. */
    MalformedSidecar("malformed_sidecar"),

    /** A peer or a restore proposed a manifest behind this device's chain head. */
    StaleRevival("stale_revival"),

    /** A write stranded by an album upgrade, held until the ceremony completes. */
    AlbumUpgradeStranded("album_upgrade_stranded"),

    /** A restored manifest whose chain diverges from the live one. Needs an explicit merge. */
    RestoreChainConflict("restore_chain_conflict"),

    /** A deposited file awaiting the provisioning user's review. */
    PendingDropReview("pending_drop_review"),
    ;

    companion object : WireCodec<QuarantineSurface>({ entries })
}

/** One item held at a [QuarantineSurface], with enough context for a human to decide. */
data class QuarantineEntry(
    /** This entry's handle. */
    val id: QuarantineId,
    /** Which refusal produced it. */
    val surface: Wire<QuarantineSurface>,
    /** The asset involved, when the refusal names one. */
    val assetId: AssetId? = null,
    /** When it was quarantined. */
    val detectedAt: Instant,
    /** The English detail message that travels with the code; deliberately not localized. */
    val detail: String,
    /** The stable code a client localizes to explain the refusal. */
    val code: ErrorCode? = null,
)
