package com.justin13888.capsule.feature.timeline

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.DayBucket
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toPersistentMap
import kotlinx.coroutines.runBlocking
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * A ready state built from the real seeded library.
 *
 * Hand-built states would let a screen test pass against tiles the repositories could never
 * produce, which is exactly the class of bug these tests exist to catch: a badge that renders
 * in a fixture and never in the app is worse than no badge at all.
 *
 * One section, because the grid's sectioning is `core:ui`'s and already pinned down by its own
 * tests — what is under test here is what a tile and its chrome say.
 *
 * @param include assets spliced in at the front, so a landmark state — quarantined, unreadable,
 *   awaiting its original — is guaranteed to be on screen rather than several thousand tiles
 *   down.
 */
fun readyState(
    mock: MockRepositories,
    assetCount: Int = 24,
    include: List<Asset> = emptyList(),
    connection: ConnectionClass = ConnectionClass.Unmetered,
    hasPendingUploads: Boolean = false,
): TimelineUiState.Ready {
    val window = runBlocking { mock.timeline.timelineWindow(0, assetCount) }
    val assets = (include + window).take(assetCount)
    val day =
        DayBucket(
            day = LocalDate.ofInstant(assets.first().captureTimestamp, ZoneOffset.UTC),
            startOffset = 0,
            count = assets.size,
        )
    val geometry =
        TimelineGeometry(
            aspects = FloatArray(assets.size) { assets[it].dimensions?.aspectRatio ?: 1f },
            sectionStarts = intArrayOf(0),
            days = persistentListOf(day),
        )
    return TimelineUiState.Ready(
        geometry = geometry,
        cells = assets.mapIndexed { index, asset -> index to asset.toTimelineCell() }.toMap().toPersistentMap(),
        connection = connection,
        hasPendingUploads = hasPendingUploads,
    )
}

/** Renders [content] inside the app's theme, pinned light so an assertion never chases a scheme. */
fun ComposeContentTestRule.launch(content: @Composable () -> Unit) {
    setContent {
        CapsuleTheme(darkTheme = false, dynamicColor = false) {
            Box(modifier = Modifier.fillMaxSize()) { content() }
        }
    }
    waitForIdle()
}
