package com.justin13888.capsule.feature.viewer.info

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.feature.viewer.ViewerFormat
import com.justin13888.capsule.feature.viewer.labelRes
import com.justin13888.capsule.feature.viewer.localizedMessage
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the information panel, whichever side of the window it lands on. */
const val ASSET_INFO_TAG: String = "viewer.info"

/**
 * Everything the app can say about one asset.
 *
 * The same composable serves two placements — a supporting pane beside the photograph on a
 * wide window, a screen of its own on a phone — because there is no width at which the
 * *content* should differ. A professional checking a datum against a coordinate needs the
 * same rows on either device; only where they sit changes.
 */
@Composable
fun AssetInfoPanel(
    state: AssetInfoUiState,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (state) {
        AssetInfoUiState.Loading ->
            CapsuleLoadingState(
                modifier = modifier.fillMaxSize().testTag(ASSET_INFO_TAG),
                contentDescription = stringResource(R.string.common_loading),
            )

        AssetInfoUiState.Missing ->
            CapsuleEmptyState(
                title = stringResource(R.string.viewer_empty_title),
                modifier = modifier.fillMaxSize().testTag(ASSET_INFO_TAG),
                body = stringResource(R.string.viewer_empty_body),
                icon = Icons.Filled.PhotoLibrary,
            )

        is AssetInfoUiState.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.viewer_error_title),
                modifier = modifier.fillMaxSize().testTag(ASSET_INFO_TAG),
                body = state.failure.localizedMessage(),
                action = {
                    CapsuleButton(onClick = onRetry, label = stringResource(R.string.viewer_action_retry))
                },
            )

        is AssetInfoUiState.Ready -> InfoSections(state.info, modifier)
    }
}

/** The panel's content, in the order a photographer reads it. */
@Composable
private fun InfoSections(
    info: AssetInfo,
    modifier: Modifier,
) {
    Column(
        modifier =
            modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .testTag(ASSET_INFO_TAG),
    ) {
        FileSection(info)
        CaptureSection(info.asset)
        CameraSection(info.asset)
        LocationSection(info.asset)
        IntegritySection(info.asset)
        LocalCopiesSection(info)
        SyncSection(info)
        CapsuleBanner(
            title = stringResource(R.string.viewer_info_privacy_note),
            modifier = Modifier.fillMaxWidth().padding(PanelPadding),
            severity = CapsuleBannerSeverity.Info,
        )
    }
}

/** Format, shape, running time and container. */
@Composable
private fun FileSection(info: AssetInfo) {
    val asset = info.asset
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_file))
    InfoRow(
        label = stringResource(R.string.viewer_info_format),
        value = asset.contentType?.raw ?: notRecorded(),
    )
    InfoRow(
        label = stringResource(R.string.viewer_info_dimensions),
        value =
            asset.dimensions?.let {
                stringResource(R.string.viewer_info_dimensions_value)
                    .withArguments("width" to it.width, "height" to it.height)
            } ?: notRecorded(),
    )
    asset.duration?.let {
        InfoRow(label = stringResource(R.string.viewer_info_duration), value = ViewerFormat.clock(it))
    }
    InfoRow(
        label = stringResource(R.string.viewer_info_album),
        value = info.albumName ?: stringResource(R.string.viewer_info_unfiled),
    )
}

/** When the shutter fired, when the file arrived, and how trustworthy the offset is. */
@Composable
private fun CaptureSection(asset: Asset) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_capture))
    InfoRow(
        label = stringResource(R.string.viewer_info_captured),
        value = ViewerFormat.timestamp(asset.captureTimestamp),
    )
    InfoRow(
        label = stringResource(R.string.viewer_info_imported),
        value = ViewerFormat.timestamp(asset.importTimestamp),
    )
    InfoRow(
        label = stringResource(R.string.viewer_info_time_zone),
        value = timeZoneSourceLabel(asset.captureTzSource),
    )
}

/**
 * The camera, with the serial marked as export-stripped rather than hidden.
 *
 * Showing it and saying it will be removed is the honest arrangement: the value is in the
 * user's own library and they are entitled to see it, while the export rule is exactly the
 * thing they need to be told before they share.
 */
@Composable
private fun CameraSection(asset: Asset) {
    val camera = asset.camera ?: return
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_camera))
    InfoRow(label = stringResource(R.string.viewer_info_camera_make), value = camera.make ?: notRecorded())
    InfoRow(label = stringResource(R.string.viewer_info_camera_model), value = camera.model ?: notRecorded())
    InfoRow(
        label = stringResource(R.string.viewer_info_camera_serial),
        value = camera.serial ?: notRecorded(),
        supporting = stringResource(R.string.viewer_info_camera_serial_stripped),
    )
}

/** The stored fix, its datum, and how it was established. */
@Composable
private fun LocationSection(asset: Asset) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_location))
    val fix = asset.gps
    if (fix == null) {
        InfoRow(
            label = stringResource(R.string.viewer_info_section_location),
            value = stringResource(R.string.viewer_info_no_location),
        )
        return
    }
    InfoRow(
        label =
            stringResource(R.string.viewer_info_coordinates).withArguments(
                "latitude" to ViewerFormat.coordinate(fix.latitude),
                "longitude" to ViewerFormat.coordinate(fix.longitude),
            ),
        value = datumLabel(fix.datum),
        supporting = gpsSourceLabel(fix.source),
    )
}

/** The plaintext digest import deduplicates on. */
@Composable
private fun IntegritySection(asset: Asset) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_integrity))
    InfoRow(
        label = stringResource(R.string.viewer_info_content_hash),
        value = ViewerFormat.shortHash(asset.contentHash.value),
    )
}

/**
 * Which rungs of the ladder this device actually holds.
 *
 * All four are listed, held or not, rather than only the ones present. "The original is not
 * here" is the fact the user came to check, and a list that omitted absent rungs would answer
 * it by silence.
 */
@Composable
private fun LocalCopiesSection(info: AssetInfo) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_local_copies))
    info.tiers.forEach { holding ->
        InfoRow(
            label = stringResource(holding.tier.labelRes()),
            value =
                stringResource(
                    if (holding.isHeld) R.string.viewer_info_tier_held else R.string.viewer_info_tier_absent,
                ),
            modifier = Modifier.testTag(tierRowTag(holding)),
        )
    }
}

/** Where the asset stands relative to the server. */
@Composable
private fun SyncSection(info: AssetInfo) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_info_section_sync))
    InfoRow(
        label = stringResource(R.string.viewer_info_section_sync),
        value = syncStateLabel(info.syncState),
        modifier = Modifier.testTag(SYNC_ROW_TAG),
    )
}

/** Test handle for one representation-tier row. */
fun tierRowTag(holding: TierHolding): String = "viewer.info.tier.${holding.tier.name}"

/** Test handle for the row describing where the asset stands relative to the server. */
const val SYNC_ROW_TAG: String = "viewer.info.sync"

private val PanelPadding = 16.dp
