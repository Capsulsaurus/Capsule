package com.justin13888.capsule.feature.recovery

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.RestoreConflict

/** What the user decided about one conflicting asset. */
enum class RestoreDecision {
    /** Nothing said yet. Behaves exactly like [KeepLocal], which is the point. */
    Undecided,

    /** The copy on this device stands. */
    KeepLocal,

    /** The user has deliberately chosen the older copy out of the archive. */
    UseBackup,
}

/**
 * What a set of merge decisions would actually do.
 *
 * Two lists rather than a count, because the sentence the screen has to be able to say is
 * "these are being rolled back" and not "some things changed".
 */
data class RestoreMergePlan(
    /** Assets whose live copy stands. */
    val keptLocal: List<AssetId>,
    /** Assets the user explicitly chose to roll back to the archived copy. */
    val rolledBack: List<AssetId>,
) {
    /** Whether this plan would change anything at all. */
    val changesAnything: Boolean get() = rolledBack.isNotEmpty()
}

/**
 * Turning conflict decisions into a plan.
 *
 * One rule, and it is the whole safety property of restore: **only an explicit
 * [RestoreDecision.UseBackup] rolls anything back.** An undecided conflict keeps the live
 * copy, exactly as a decided-to-keep one does. That is what makes a six-month-old archive
 * unable to resurrect an asset deleted last week, and it is what makes walking away from this
 * screen a safe thing to do rather than an unfinished one.
 *
 * A pure function over two values, so the rule is tested directly rather than inferred from
 * what a screen happened to render.
 */
object RestoreMerge {
    /** What [decisions] would do to [conflicts]. */
    fun plan(
        conflicts: List<RestoreConflict>,
        decisions: Map<String, RestoreDecision>,
    ): RestoreMergePlan {
        val rolledBack = mutableListOf<AssetId>()
        val keptLocal = mutableListOf<AssetId>()
        conflicts.forEach { conflict ->
            val decision = decisions[conflict.assetId.value] ?: RestoreDecision.Undecided
            if (decision == RestoreDecision.UseBackup) rolledBack += conflict.assetId else keptLocal += conflict.assetId
        }
        return RestoreMergePlan(keptLocal = keptLocal, rolledBack = rolledBack)
    }

    /** How many conflicts nobody has said anything about yet. */
    fun undecidedCount(
        conflicts: List<RestoreConflict>,
        decisions: Map<String, RestoreDecision>,
    ): Int =
        conflicts.count { conflict ->
            (decisions[conflict.assetId.value] ?: RestoreDecision.Undecided) == RestoreDecision.Undecided
        }
}
