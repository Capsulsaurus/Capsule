package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.collections.immutable.toImmutableList
import java.time.Duration

/** Drawn behind a tile that has no placeholder of its own. */
private const val NEUTRAL_PLACEHOLDER = 0x2A2A2A

private const val SECONDS_PER_MINUTE = 60L

private const val MINUTES_PER_HOUR = 60L

/**
 * Turns catalog records into what the People screens draw.
 *
 * Pure and separate from the view models, because the decisions worth checking are here:
 * which groupings are being withheld from queries, how many outputs that adds up to, and
 * which suggestions a person's screen is allowed to offer.
 */
object PeopleProjection {
    /** The tile for one asset. */
    fun photoOf(asset: Asset): PersonPhoto =
        PersonPhoto(
            assetId = asset.id.value,
            // A record the extractor could not probe still occupies a cell, and a square is
            // the least wrong guess: it biases neither portrait nor landscape.
            aspectRatio = asset.dimensions?.aspectRatio?.takeIf { it > 0f } ?: 1f,
            placeholder =
                asset.lqip?.dominantColor?.let { (it.red shl 16) or (it.green shl 8) or it.blue }
                    ?: NEUTRAL_PLACEHOLDER,
            videoDuration = asset.duration?.let(::formatDuration),
        )

    /**
     * The groupings that are not answering queries, or `null` when every slot is canonical.
     *
     * A superseded grouping is kept rather than deleted — the originals are still there to
     * regenerate from — but it must not match a query, because embeddings from two model
     * versions live in different vector spaces and comparing across them is meaningless.
     */
    fun staleNotice(clusters: List<FaceCluster>): StaleClusterNotice? {
        val stale = clusters.filter { it.status.knownOrNull != ModelOutputStatus.Current }
        if (stale.isEmpty()) return null
        return StaleClusterNotice(
            staleCount = stale.size,
            slots =
                stale
                    .map(FaceCluster::slot)
                    .map(::spell)
                    .distinct()
                    .sorted()
                    .toImmutableList(),
        )
    }

    /**
     * The current model suggestions across a set of photographs, most repeated first.
     *
     * Only suggestions on the canonical model slot: `freshAi` drops the rest. Offering a
     * superseded suggestion for promotion would let a value the library has already stopped
     * trusting be written into the user's own namespace, which is the one direction the
     * AI/user separation exists to prevent.
     */
    fun suggestions(
        photos: List<AssetId>,
        tags: Map<AssetId, AssetTags>,
        limit: Int,
    ): List<AiSuggestion> =
        photos
            .flatMap { assetId ->
                tags[assetId]?.freshAi.orEmpty().map { aiTag -> assetId to aiTag }
            }.sortedWith(compareBy({ it.second.tag }, { it.first.value }))
            .distinctBy { it.second.tag }
            .take(limit)
            .map { (assetId, aiTag) -> suggestionOf(assetId, aiTag) }

    /**
     * Resolves a repository failure to something a screen can say out loud.
     *
     * Collapsed here rather than published so the screen has an exhaustive `when` and no
     * unrecognised `error.*` code can reach a user as a raw token.
     */
    fun failureOf(error: CapsuleError): PeopleFailure =
        when (error) {
            is LocalError.Offline -> PeopleFailure.Offline
            is LocalError.NotFound -> PeopleFailure.Missing
            is ModerationError.AccountSuspended -> PeopleFailure.Suspended
            is QuotaError.GraceLocked, is QuotaError.Exceeded -> PeopleFailure.QuotaLocked
            is ProtocolError.VersionUnsupported -> PeopleFailure.Unsupported
            else -> PeopleFailure.Unknown
        }
}

private fun suggestionOf(
    assetId: AssetId,
    aiTag: AiTag,
): AiSuggestion =
    AiSuggestion(
        assetId = assetId.value,
        tag = aiTag.tag,
        deviceId = aiTag.addId.deviceId.value,
        counter = aiTag.addId.counter,
    )

/** `model-id version`, the spelling a stale notice lists slots by. */
private fun spell(slot: ModelSlot): String = "${slot.modelId.value} ${slot.modelVersion.value}"

/**
 * A running time as `m:ss`, or `h:mm:ss` once it passes an hour.
 *
 * Formatted here rather than in the composition because the gallery's cell chrome takes an
 * already-formatted string: it draws the marker and owns the announced label, and has no
 * business knowing what a `Duration` is.
 */
private fun formatDuration(duration: Duration): String {
    val total = duration.seconds.coerceAtLeast(0)
    val seconds = total % SECONDS_PER_MINUTE
    val minutes = (total / SECONDS_PER_MINUTE) % MINUTES_PER_HOUR
    val hours = total / (SECONDS_PER_MINUTE * MINUTES_PER_HOUR)
    return if (hours > 0) {
        "%d:%02d:%02d".format(hours, minutes, seconds)
    } else {
        "%d:%02d".format(minutes, seconds)
    }
}
