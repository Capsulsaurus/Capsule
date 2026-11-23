package com.justin13888.capsule.feature.viewer.info

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.CaptureTzSource
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadTier
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.feature.viewer.ViewerFormat
import com.justin13888.capsule.feature.viewer.withArguments

/**
 * One labelled fact.
 *
 * A list row rather than a two-column grid because the panel is read at every width from a
 * 412dp phone to a supporting pane beside a photograph, and a grid that fits the first
 * either wraps or truncates in the second. The label is the headline and the value the
 * supporting line, so a screen reader announces "Datum, WGS-84" rather than two orphaned
 * strings.
 */
@Composable
internal fun InfoRow(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
) {
    CapsuleListItem(
        headline = label,
        modifier = modifier,
        supporting = listOfNotNull(value, supporting).joinToString(separator = "\n"),
    )
}

/** The value shown where the sidecar carries nothing at all. */
@Composable
internal fun notRecorded(): String = stringResource(R.string.viewer_info_unknown)

/**
 * How the capture-time offset from UTC was established.
 *
 * An unrecognised token is rendered as itself rather than folded into "not recorded". The
 * three sources are not equally trustworthy and a fourth written by a newer peer is a fact
 * about the file worth showing, not noise to hide.
 */
@Composable
internal fun timeZoneSourceLabel(source: Wire<CaptureTzSource>?): String =
    when (source) {
        null -> notRecorded()
        is Wire.Unknown -> source.raw
        is Wire.Known ->
            when (source.value) {
                CaptureTzSource.OffsetExif -> stringResource(R.string.viewer_info_time_zone_offset_exif)
                CaptureTzSource.GpsLookup -> stringResource(R.string.viewer_info_time_zone_gps_lookup)
                CaptureTzSource.Floating -> stringResource(R.string.viewer_info_time_zone_floating)
            }
    }

/**
 * The datum a stored fix is expressed in.
 *
 * Named explicitly, and the GCJ-02 case says out loud that plotting it on a WGS-84 map is
 * approximate. Coordinates are stored verbatim in whatever datum the source supplied and
 * never converted at rest, so the only place the difference can be explained is here.
 */
@Composable
internal fun datumLabel(datum: Wire<GpsDatum>): String =
    when (datum) {
        is Wire.Unknown -> stringResource(R.string.viewer_info_datum_unknown).withArguments("token" to datum.raw)
        is Wire.Known ->
            when (datum.value) {
                GpsDatum.Wgs84 -> stringResource(R.string.viewer_info_datum_wgs84)
                GpsDatum.Gcj02 -> stringResource(R.string.viewer_info_datum_gcj02)
            }
    }

/**
 * How a fix was established.
 *
 * The inferred case is worded as a pending suggestion because that is exactly what it is: an
 * inferred coordinate is written to the canonical field only on explicit user confirmation,
 * the same promotion rule that governs a suggested tag becoming a user tag.
 */
@Composable
internal fun gpsSourceLabel(source: Wire<GpsSource>): String =
    when (source) {
        is Wire.Unknown -> source.raw
        is Wire.Known ->
            when (source.value) {
                GpsSource.Exif -> stringResource(R.string.viewer_info_gps_source_exif)
                GpsSource.Manual -> stringResource(R.string.viewer_info_gps_source_manual)
                GpsSource.Inferred -> stringResource(R.string.viewer_info_gps_source_inferred)
            }
    }

/**
 * Where the asset stands relative to the server, in one sentence.
 *
 * The awaiting-original arm reads as progress rather than failure on purpose: it is derived
 * from the feed's `original_held` flag, is explicitly distinct from a permanent loss, and
 * flips by itself when the uploading device finishes.
 */
@Composable
internal fun syncStateLabel(state: SyncState): String =
    when (state) {
        SyncState.Synced -> stringResource(R.string.viewer_info_sync_synced)
        SyncState.LocalOnly -> stringResource(R.string.viewer_info_sync_local_only)
        SyncState.AwaitingOriginal -> stringResource(R.string.viewer_info_sync_awaiting_original)
        is SyncState.Quarantined -> stringResource(R.string.viewer_info_sync_quarantined)
        is SyncState.Failed -> stringResource(R.string.viewer_info_sync_failed)
        is SyncState.Queued ->
            stringResource(R.string.viewer_info_sync_queued)
                .withArguments("tier" to uploadTierLabel(state.nextTier))

        is SyncState.Uploading ->
            stringResource(R.string.viewer_info_sync_uploading).withArguments(
                "tier" to uploadTierLabel(state.tier),
                "percent" to ViewerFormat.percent(state.bytesSent, state.bytesTotal),
            )
    }

/** The catalogue name for one rung of the upload ladder. */
@Composable
internal fun uploadTierLabel(tier: UploadTier): String =
    when (tier) {
        UploadTier.Index -> stringResource(R.string.viewer_upload_tier_index)
        UploadTier.Preview -> stringResource(R.string.viewer_upload_tier_preview)
        UploadTier.Original -> stringResource(R.string.viewer_upload_tier_original)
    }
