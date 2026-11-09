package com.justin13888.capsule.screens.labels

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.AssetRegion
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.LabelRefusal
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.RegionShape
import com.justin13888.capsule.screens.domain.ShapeRefusal
import com.justin13888.capsule.screens.domain.ShapeRefusalReason
import com.justin13888.capsule.screens.domain.UserLabel
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiLabelRefusal
import uniffi.capsule_sdk.FfiLabelTarget
import uniffi.capsule_sdk.FfiRegion
import uniffi.capsule_sdk.FfiRegionShape
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.util.logging.Level
import java.util.logging.Logger

/**
 * [LabelPort] over the workspace's label and region verbs (`FfiWorkspace::labels`,
 * `add_label`, `remove_label`, `regions`, `add_region`, `delete_region`).
 *
 * The workspace decides everything: this converts the ids and shapes to the flat FFI
 * values, and the workspace's refusals (`FfiError::Label`) back to [LabelRefusal],
 * [ShapeRefusal] and [LabelTargetNotFound]. A label is put through [UserLabel.normalize]
 * first, so a label the rule refuses is refused before an unknown target, as the port
 * promises, and before anything crosses the boundary. An id that is not a UUID names
 * nothing the workspace could hold, so it is [LabelTargetNotFound] too.
 *
 * The verbs are blocking - each reads or rewrites a sidecar register on disk - so they
 * run on [io]. Any other workspace failure is rethrown as [IllegalStateException], and so
 * is a core that cannot be reached (a [LinkageError]: no `libcapsule_sdk.so` or no JNA
 * dispatch library), which the editor shows as an unreachable store rather than a crash.
 */
class FfiLabelPort(
    private val workspace: FfiWorkspaceInterface,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) : LabelPort {
    override suspend fun labels(target: LabelTarget): List<String> = call("labels") { workspace.labels(target.toFfi()) }

    override suspend fun addLabel(
        label: String,
        target: LabelTarget,
    ): List<String> {
        val normalized = UserLabel.normalize(label)
        return call("add label") { workspace.addLabel(target.toFfi(), normalized) }
    }

    override suspend fun removeLabel(
        label: String,
        target: LabelTarget,
    ): List<String> {
        val normalized = UserLabel.normalize(label)
        return call("remove label") { workspace.removeLabel(target.toFfi(), normalized) }
    }

    override suspend fun regions(asset: AssetId): List<AssetRegion> =
        call("regions") { workspace.regions(asset.value).map { it.toRegion() } }

    override suspend fun addRegion(
        shape: RegionShape,
        labels: List<String>,
        asset: AssetId,
    ): AssetRegion {
        val normalized = labels.map(UserLabel::normalize)
        return call("add region") { workspace.addRegion(asset.value, shape.toFfi(), normalized).toRegion() }
    }

    override suspend fun deleteRegion(
        region: RegionId,
        asset: AssetId,
    ) {
        call("delete region") { workspace.deleteRegion(asset.value, region.value) }
    }

    private suspend fun <T> call(
        verb: String,
        body: () -> T,
    ): T =
        withContext(io) {
            try {
                body().also { log.fine("labels: $verb answered") }
            } catch (error: FfiException.Label) {
                log.fine("labels: $verb refused (${error.refusal})")
                throw error.refusal.toDomain().apply { initCause(error) }
            } catch (error: FfiException.InvalidArgument) {
                log.fine("labels: $verb named an id that is not a UUID")
                throw LabelTargetNotFound().apply { initCause(error) }
            } catch (error: FfiException) {
                log.log(Level.WARNING, "labels: $verb failed in the workspace", error)
                throw IllegalStateException("the workspace could not $verb", error)
            } catch (error: LinkageError) {
                log.log(Level.WARNING, "labels: the core's native library is unavailable", error)
                throw IllegalStateException("the workspace's native library is unavailable", error)
            }
        }

    companion object {
        private val log: Logger = Logger.getLogger("capsule.labels")

        /** The domain's refusal for the workspace's. */
        fun FfiLabelRefusal.toDomain(): RuntimeException =
            when (this) {
                is FfiLabelRefusal.NotFound -> LabelTargetNotFound()
                is FfiLabelRefusal.LabelEmpty -> LabelRefusal.Empty()
                is FfiLabelRefusal.LabelTooLong -> LabelRefusal.TooLong(bytes.coerceAtMost(Int.MAX_VALUE.toULong()).toInt())
                is FfiLabelRefusal.RegionOutOfRange -> ShapeRefusal(ShapeRefusalReason.OUT_OF_RANGE)
                is FfiLabelRefusal.RegionEmpty -> ShapeRefusal(ShapeRefusalReason.EMPTY)
                is FfiLabelRefusal.RegionPastEdge -> ShapeRefusal(ShapeRefusalReason.PAST_EDGE)
            }

        fun LabelTarget.toFfi(): FfiLabelTarget =
            when (this) {
                is LabelTarget.Region -> FfiLabelTarget.Region(asset.value, region.value)
                is LabelTarget.Stack -> FfiLabelTarget.Stack(stack.value)
                is LabelTarget.Album -> FfiLabelTarget.Album(album.value)
            }

        fun RegionShape.toFfi(): FfiRegionShape =
            when (this) {
                is RegionShape.Circle -> FfiRegionShape.Circle(centerX, centerY, radius)
                is RegionShape.Rect -> FfiRegionShape.Rect(left, top, width, height)
            }

        fun FfiRegion.toRegion(): AssetRegion =
            AssetRegion(
                RegionId(regionId),
                when (val outline = shape) {
                    is FfiRegionShape.Circle -> RegionShape.Circle(outline.cx, outline.cy, outline.r)
                    is FfiRegionShape.Rect -> RegionShape.Rect(outline.x, outline.y, outline.w, outline.h)
                },
                labels,
            )
    }
}
