package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId

/**
 * The cull verbs of the library (*Organization - Culling*): read the pass, flag an asset or
 * a whole stack, and move the rejects to the trash.
 *
 * Flagging is metadata-only and reversible; moving to the trash is the one destructive step,
 * and it is a soft delete the library can restore until its retention window closes. A
 * failure is any exception the adapter throws; [SweepIncomplete] says which assets a sweep
 * moved before it stopped.
 */
interface CullPort {
    /** The live assets of the pass, in sequence order, each with its flag and stack. */
    suspend fun pass(): List<CullAsset>

    /** Flag one asset. [CullFlag.NEUTRAL] clears a prior flag. */
    suspend fun setCull(
        asset: AssetId,
        flag: CullFlag,
    )

    /**
     * Flag every live member of [stack], and return the members flagged: every one the
     * library holds, whether or not the pass has read it.
     */
    suspend fun flagStack(
        stack: StackId,
        flag: CullFlag,
    ): List<AssetId>

    /**
     * Move exactly [assets] to the trash, and return those moved. Throws [SweepIncomplete]
     * when it stops partway, naming the ones already moved: they stay in the trash.
     */
    suspend fun moveToTrash(assets: List<AssetId>): List<AssetId>
}

/** A sweep that stopped partway: [moved] reached the trash, the rest did not. */
class SweepIncomplete(
    val moved: List<AssetId>,
    cause: Throwable? = null,
) : IllegalStateException("the sweep stopped after ${moved.size} assets", cause)
