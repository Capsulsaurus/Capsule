package com.justin13888.capsule.feature.viewer

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.isKnown

/**
 * A way this asset is less than fully available, without anything being lost.
 *
 * Every member is **non-destructive by construction**: not one of them removes the asset from
 * the library, drops a field, or edits a byte. That is why they are a separate axis from
 * [ViewerFailure] rather than error states — a failure is something that went wrong and can
 * be retried, whereas these are conditions the library is legitimately in and must keep
 * working under.
 *
 * They are also deliberately *distinct*, not a single "degraded" flag. Each one has a
 * different cause, a different remedy, and a different answer to "is my photo safe?", and a
 * user shown one amber banner for all five would learn to ignore all five.
 */
enum class ViewerDegradation {
    /**
     * A permanent failure fetching a rung above what is held — `410 Gone`, a purged origin,
     * an unreachable home server.
     *
     * The fetch walked *down* the ladder to the best rung in hand and stopped at the
     * always-present placeholder. The asset stays listed and re-fetches by itself once the
     * representation becomes reachable again.
     */
    FullResolutionUnavailable,

    /**
     * The original has not left the capturing device yet — the derived `awaiting-original`
     * state, `error.blob.pending_upload`.
     *
     * Explicitly not a durability loss and explicitly distinct from a `410`: the client shows
     * a badge, never a failure, and re-fetches when the sync feed flips `original_held`.
     */
    OriginalOnAnotherDevice,

    /**
     * No decoder on this device handles the file, after the sandbox exhausted its retries.
     *
     * The bytes and every recorded fact about them are untouched; only the pixels cannot be
     * produced here. Another device with the right decoder shows the same asset normally.
     */
    UnreadableHere,

    /**
     * The asset carries tokens outside this build's closed enum sets — a peer running a later
     * `protocol_version` wrote it.
     *
     * Editing is refused rather than the asset being hidden. This client cannot re-sign the
     * sidecar without either dropping a field it does not understand, which invalidates the
     * signature, or guessing at a value it has no basis to guess at. Showing it read-only is
     * the only honest option; hiding it would look like data loss.
     */
    CreatedByNewerVersion,

    /**
     * Inbound state was refused and is waiting for a human decision.
     *
     * Never silently dropped and never silently applied. What is on screen is this device's
     * own state, unaltered, while the refused change waits to be inspected.
     */
    Quarantined,
    ;

    /**
     * Whether this condition forbids writing to the asset.
     *
     * Only the forward-schema case does. A quarantined asset's *local* state is perfectly
     * writable — the refusal is about what a peer proposed, not about what the user may do —
     * and conflating the two would lock a user out of their own photo over someone else's
     * bad manifest.
     */
    val blocksEditing: Boolean get() = this == CreatedByNewerVersion
}

/**
 * The conditions an asset record alone establishes, in the order they are drawn.
 *
 * Derived rather than stored. Each of these is already a fact about the asset — an unknown
 * wire token, a quarantine surface on its sync state — and recording a second copy under a
 * `degraded` flag would create a source of truth able to disagree with the first.
 *
 * [ViewerDegradation.FullResolutionUnavailable] is deliberately absent: it is a property of a
 * *fetch attempt*, not of the record, so it is contributed by [RepresentationStatus] instead.
 */
internal fun intrinsicDegradations(asset: Asset): List<ViewerDegradation> =
    buildList {
        if (asset.syncState is SyncState.Quarantined) add(ViewerDegradation.Quarantined)
        if (isForwardSchema(asset)) add(ViewerDegradation.CreatedByNewerVersion)
        if (isUnreadable(asset)) add(ViewerDegradation.UnreadableHere)
        if (asset.syncState == SyncState.AwaitingOriginal) add(ViewerDegradation.OriginalOnAnotherDevice)
    }

/**
 * Whether a newer `protocol_version` produced this record.
 *
 * Checks exactly the fields a write would have to re-encode. A closed-enum token this build
 * does not know is preserved verbatim so a round-trip keeps the peer's signature valid, but
 * it can never be allowed to drive behaviour — including the behaviour of re-signing it.
 */
internal fun isForwardSchema(asset: Asset): Boolean =
    !asset.assetType.isKnown ||
        !asset.cull.isKnown ||
        asset.captureTzSource?.isKnown == false

/**
 * Whether this device can produce pixels for the file at all.
 *
 * An unrecognised `content_type` is the honest signal: the format is outside the closed set
 * this build knows, so no decoder here claims it. The record keeps the token verbatim, and a
 * device that does understand the format renders the same asset normally.
 */
internal fun isUnreadable(asset: Asset): Boolean = asset.contentType?.isKnown == false

/**
 * The best rung this device can draw for [asset], never worse than the placeholder.
 *
 * The ladder always has a floor: the placeholder rides inside the encrypted metadata blob and
 * arrives with metadata at zero extra request, which is exactly why a missing derivative is a
 * display problem rather than a blank tile.
 */
internal fun drawableTier(asset: Asset): RepresentationTier = asset.heldRepresentations.best
