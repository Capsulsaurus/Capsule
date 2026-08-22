package com.justin13888.capsule.core.model

/**
 * Whether a model-derived value is still comparable against what the current model produces.
 *
 * Three outcomes, not two, and the middle one is the reason: an output from a *known but
 * superseded* model version is kept and excluded from queries until it is regenerated, while
 * an output from a model the inventory does not recognise at all is refused outright. Folding
 * them together would either discard a perfectly regenerable queue or admit values from a
 * model nobody can account for.
 */
enum class ModelOutputStatus(
    override val wire: String,
) : WireEnum {
    /** Produced by the canonical model for its task. Queryable. */
    Current("current"),

    /** Produced by a known, superseded version. Excluded from queries, queued to regenerate. */
    Stale("stale"),

    /** Produced by a model the inventory does not know. Refused, never admitted. */
    Unrecognised("unrecognised"),
    ;

    /** Whether values with this status participate in query results. */
    val isQueryable: Boolean get() = this == Current

    companion object : WireCodec<ModelOutputStatus>({ entries })
}

/**
 * A grouping of face detections the clustering pass produced.
 *
 * **Nothing here is durable state.** A cluster is a computed view: its membership is a pure
 * function of the input asset set and the `(model_id, model_version)` slot that produced the
 * embeddings, processed in sorted asset-id order with fixed seeds, so recomputing it yields
 * byte-identical output on every device. This type is a snapshot of that computation for the
 * UI to render, not a record to reconcile — which is why it has no timestamps, no stamps, and
 * no merge.
 */
data class FaceCluster(
    /** The cluster's handle within the current computation. */
    val id: FaceClusterId,
    /** Which model produced the embeddings behind it. */
    val slot: ModelSlot,
    /** Whether the slot is still canonical. A stale cluster is not queried. */
    val status: Wire<ModelOutputStatus>,
    /** The assets in the cluster, in sorted asset-id order. */
    val assetIds: List<AssetId>,
    /** The asset to show as the cluster's face. */
    val coverAssetId: AssetId? = null,
    /** The person the user attached to this cluster, if any. */
    val personId: PersonId? = null,
)

/**
 * A person the user named.
 *
 * The name is the user's contribution and the clusters are the model's; keeping them in
 * separate fields is the same structural separation that keeps AI tags out of user tags. A
 * re-clustering can replace every [clusterIds] entry without touching what the user typed.
 *
 * Face matches to a named person are personal contact data and are stripped when an asset
 * crosses a trust boundary.
 */
data class Person(
    /** The person's handle. */
    val id: PersonId,
    /** What the user called them. */
    val displayName: String,
    /** The clusters the user has attached to this person. */
    val clusterIds: List<FaceClusterId> = emptyList(),
    /** The asset to show as this person's face. */
    val coverAssetId: AssetId? = null,
    /** How many assets they appear in across every attached cluster. */
    val assetCount: Int = 0,
    /** Whether this person is surfaced in the People view. */
    val isFavourite: Boolean = false,
)
