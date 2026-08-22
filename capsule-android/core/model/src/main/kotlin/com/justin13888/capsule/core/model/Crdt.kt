package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * The stamp that totally orders concurrent writes to a last-writer-wins register.
 *
 * [writtenAt] alone is not a total order — two devices genuinely do caption the same photo
 * within the same millisecond — so the writing device id breaks the tie lexicographically.
 * A total order is what makes the merge commutative, and a commutative merge is what lets a
 * peer replay a stale operation without corrupting current state.
 */
data class LwwStamp(
    /** The signed timestamp of the write. */
    val writtenAt: Instant,
    /** The writing device, the lexicographic tiebreaker on an exact timestamp tie. */
    val by: DeviceId,
) : Comparable<LwwStamp> {
    override fun compareTo(other: LwwStamp): Int {
        val byTime = writtenAt.compareTo(other.writtenAt)
        return if (byTime != 0) byTime else by.value.compareTo(other.by.value)
    }
}

/**
 * A last-writer-wins register over a value that may be absent.
 *
 * The value domain deliberately includes "no value": leaving a stack, clearing a caption,
 * and deleting a smart album are all a stamped absence rather than a separate delete
 * operation, so every mutation is one write and converges by the same rule. A separate
 * delete channel would need its own conflict story, and there is no good one.
 */
data class Lww<T : Any>(
    /** The register's current value, or `null` for a stamped absence (a tombstone). */
    val value: T?,
    /** When and by whom it was written. */
    val stamp: LwwStamp,
) {
    /**
     * Merges [other] into this register, keeping the higher stamp.
     *
     * Idempotent, commutative and associative by construction: merging in any order, or
     * merging the same operation twice, lands on the same value.
     */
    fun merge(other: Lww<T>): Lww<T> = if (other.stamp > stamp) other else this

    /** Whether this register currently holds a tombstone rather than a value. */
    val isTombstone: Boolean get() = value == null
}

/**
 * The identity of one add to an observed-remove set — `(device_id, monotonic_counter)`.
 *
 * Every OR-set add carries one and every remove targets one, which is what makes "remove a
 * tag you never added" a rejection rather than a silent no-op. The counter must never repeat
 * for a given `(device, asset, set)`: a reused value would alias two distinct adds, so
 * removing one would silently delete the other.
 */
data class AddId(
    /** The adding device — UUIDv4. */
    val deviceId: DeviceId,
    /** Strictly increasing per device, per asset, per set, across restarts and reinstalls. */
    val counter: Long,
)

/**
 * A tag the user wrote — one entry of the `tags_user` OR-set.
 *
 * Structurally separate from [AiTag] rather than distinguished by a flag on one type. The
 * separation is what makes "can an AI tag overwrite a user tag?" a question that cannot
 * arise: they are different fields, so a hallucinating model cannot pollute user intent.
 */
data class UserTag(
    /** The tag text. */
    val tag: String,
    /** Which add this entry is, so a remove can name it. */
    val addId: AddId,
)

/**
 * The `(model_id, model_version)` slot an AI-derived value came from.
 *
 * Vector spaces differ across pairs, so outputs are not comparable across slots. Carrying
 * the pair on every entry is what lets a client tell "produced by the model we run today"
 * from "produced by a model we have since swapped", instead of silently comparing across
 * versions and returning nonsense.
 */
data class ModelSlot(
    /** Stable across versions, e.g. `mobileclip-b`. */
    val modelId: ModelId,
    /** Bumped on every swap for the task. */
    val modelVersion: ModelVersion,
)

/**
 * A tag a model suggested — one entry of the `tags_ai` OR-set.
 *
 * Promotion to a [UserTag] is an explicit, signed user action that copies the entry with a
 * fresh user-scoped [AddId]; it is never automatic. Dismissal is an OR-set remove keyed by
 * this entry's [addId].
 */
data class AiTag(
    /** The tag text. */
    val tag: String,
    /** Which add this entry is. */
    val addId: AddId,
    /** Which model produced it. */
    val slot: ModelSlot,
    /**
     * Whether the canonical model for [slot] has since changed.
     *
     * A stale entry is excluded from queries and queued for regeneration — it is **not**
     * deleted, because the original is still there to regenerate from and throwing the entry
     * away would lose the fact that something was once suggested here.
     */
    val isStale: Boolean = false,
)

/**
 * A caption that lost a concurrent edit, kept so the loss is visible.
 *
 * A plain last-writer-wins register drops one side of a tied edit silently, which is real
 * data loss when two people caption the same photo seconds apart. Keeping the displaced
 * value turns that into a recoverable surface: the UI can offer "this replaced another
 * caption" and let the user restore it.
 */
data class SupersededCaption(
    /** The displaced text. */
    val value: String,
    /** The device that wrote it. */
    val writtenBy: DeviceId,
    /** When it was written. */
    val writtenAt: Instant,
)

/**
 * An asset's caption — the winning register plus the values it displaced.
 *
 * A client must never strip [superseded]; doing so would discard the only record that a
 * concurrent edit happened at all.
 */
data class Caption(
    /** The authoritative value, or a stamped absence when the caption was cleared. */
    val current: Lww<String>?,
    /** Displaced values, oldest evicted first once the cap is reached. */
    val superseded: List<SupersededCaption> = emptyList(),
) {
    companion object {
        /** The cap on retained superseded captions; the oldest is evicted past it. */
        const val MAX_SUPERSEDED = 16

        /** The cap on a caption's encoded length. */
        const val MAX_VALUE_BYTES = 4096
    }
}

/**
 * Both of an asset's tag sets, side by side.
 *
 * Returned together because the UI shows them together and a caller that fetched only one
 * would have no way to render the promote/dismiss affordances that only make sense in
 * contrast. They stay separate *fields* for the same reason they are separate on the wire:
 * a model can never write into [user].
 */
data class AssetTags(
    /** Tags the user wrote. */
    val user: List<UserTag> = emptyList(),
    /** Tags a model suggested, each carrying the slot that produced it. */
    val ai: List<AiTag> = emptyList(),
) {
    /** AI tags still comparable against the current model — the ones worth showing. */
    val freshAi: List<AiTag> get() = ai.filterNot(AiTag::isStale)
}
