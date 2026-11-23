package com.justin13888.capsule.feature.viewer.info

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.feature.viewer.ViewerDegradation
import com.justin13888.capsule.feature.viewer.ViewerFailure
import com.justin13888.capsule.feature.viewer.intrinsicDegradations
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList

/** One rung of the representation ladder and whether this device has it. */
@Immutable
data class TierHolding(
    /** The rung. */
    val tier: RepresentationTier,
    /** Whether it can be served without a request. */
    val isHeld: Boolean,
)

/**
 * Everything the panel says about one asset.
 *
 * Assembled from four contracts that fail independently — the record, what the cache holds,
 * where the server stands, and which album contains it — and deliberately *not* from a
 * widened `Asset`. What a catalog query returns and what a viewer wants to display are two
 * different sets, and folding the second into the first would make the domain type lie about
 * the first.
 */
@Immutable
data class AssetInfo(
    /** The record itself. */
    val asset: Asset,
    /** Every rung, held or not, cheapest first. */
    val tiers: ImmutableList<TierHolding>,
    /** Where the asset stands relative to the server. */
    val syncState: SyncState,
    /** The container album's name, or `null` when it is unfiled or nameless. */
    val albumName: String?,
    /** The conditions the record alone establishes. */
    val degradations: ImmutableList<ViewerDegradation>,
) {
    companion object {
        /** Builds the panel's value from the four reads that feed it. */
        fun of(
            asset: Asset,
            held: HeldRepresentations,
            syncState: SyncState,
            albumName: String?,
        ): AssetInfo =
            AssetInfo(
                asset = asset,
                tiers =
                    RepresentationTier.entries
                        .sortedBy(RepresentationTier::rank)
                        .map { TierHolding(it, held.holds(it)) }
                        .toImmutableList(),
                syncState = syncState,
                albumName = albumName?.takeIf(String::isNotBlank),
                degradations = intrinsicDegradations(asset).toImmutableList(),
            )
    }
}

/** What the information panel is showing. */
@Immutable
sealed interface AssetInfoUiState {
    /** The reads have not landed yet. */
    data object Loading : AssetInfoUiState

    /** The asset is not in this library. */
    data object Missing : AssetInfoUiState

    /** One of the reads failed with a code the user can act on. */
    data class Failed(
        val failure: ViewerFailure,
    ) : AssetInfoUiState

    /** Everything there is to say. */
    data class Ready(
        val info: AssetInfo,
    ) : AssetInfoUiState
}
