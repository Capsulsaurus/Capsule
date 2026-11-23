package com.justin13888.capsule.feature.sync

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadTier
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Instant

/**
 * The upload ladder and the fetch queue beneath it.
 *
 * The ladder's three rungs are asserted to be present even when empty, because that is what
 * lets a user on a metered link see that the *index* of their library has escaped the device
 * while the originals have not — the single fact staged uploads exist to deliver.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class TransferQueueScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `all three rungs are named, empty ones included`() {
        render(loaded())

        compose.onNodeWithTag(UPLOAD_QUEUE_TAG).assertExists()
        // Each rung's own body copy, which appears once per rung — the tier label itself also
        // headlines every row at that rung, so matching on it would match more than the header.
        compose
            .onNodeWithText(
                "The signed manifest and the details blob. Goes out on any usable connection, " +
                    "so what exists escapes this device first.",
            ).assertExists()
        compose
            .onNodeWithText("Thumbnail and screen-sized preview. Waits for a connection that is not metered.")
            .assertExists()
        compose
            .onNodeWithText(
                "The original file. Waits for unmetered Wi-Fi, or for a sync you start yourself.",
            ).assertExists()
    }

    @Test
    fun `a rung the connection will not open says it is held back`() {
        render(loaded(connection = ConnectionClass.Metered))

        compose.onNodeWithText("Held back by this connection").assertExists()
    }

    @Test
    fun `an empty ladder says nothing is owed rather than showing three blank sections`() {
        render(loaded(uploads = emptyList()))

        compose.onNodeWithText("Nothing to upload").assertExists()
    }

    @Test
    fun `a pending original states that the local copy stays put until it lands`() {
        render(loaded())

        compose.onNodeWithText("Its original stays on this device until the upload is confirmed").assertExists()
    }

    @Test
    fun `a failed rung localizes its code rather than showing a status`() {
        render(
            loaded(
                uploads =
                    listOf(
                        entry(UploadTier.Original, TierProgress.Failed, ErrorCode("error.upload.checksum_mismatch")),
                    ),
            ),
        )

        compose.onNodeWithText("Upload data was corrupted in transit. Retrying…").assertExists()
    }

    @Test
    fun `a fetch row names the rung it wants and the rung it holds`() {
        render(loaded())

        compose.onNodeWithText("Wants Original, holds Thumbnail").assertExists()
    }

    @Test
    fun `fetching one asset reaches the caller`() {
        val fetched = mutableListOf<AssetId>()
        render(loaded(), actions(onFetchNow = { fetched += it }))

        compose.onNodeWithText("Fetch now").performClick()
        compose.waitForIdle()

        assertEquals(listOf(AssetId("fetch")), fetched)
    }

    @Test
    fun `a degraded row earns the banner that says nothing was removed`() {
        render(loaded(degraded = true))

        compose.onNodeWithText("Some originals could not be fetched").assertExists()
    }

    @Test
    fun `an empty fetch queue says the scope is satisfied`() {
        render(loaded(fetches = emptyList()))

        compose.onNodeWithTag(FETCH_QUEUE_TAG).assertExists()
        compose.onNodeWithText("Nothing to fetch").assertExists()
    }

    @Test
    fun `a first read that fails offers a way back`() {
        var retried = false
        render(
            state = TransferQueueUiState(isLoading = true, failure = ErrorCode("error.sync.unauthenticated")),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("Could not read sync state").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    private fun render(
        state: TransferQueueUiState,
        actions: TransferQueueActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { TransferQueueScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    private fun actions(
        onFetchNow: (AssetId) -> Unit = {},
        onRetry: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = TransferQueueActions(onFetchNow, onRetry, onBack)

    private fun loaded(
        connection: ConnectionClass = ConnectionClass.Unmetered,
        uploads: List<UploadQueueEntry> = listOf(entry(UploadTier.Original, TierProgress.Queued)),
        fetches: List<FetchQueueEntry> = listOf(fetch()),
        degraded: Boolean = false,
    ) = TransferQueueUiState(
        isLoading = false,
        connection = connection,
        uploads =
            UploadTier.entries
                .map { tier ->
                    UploadTierGroup(
                        tier = tier,
                        entries = uploads.filter { it.tier == tier }.toImmutableList(),
                        opensOnThisConnection = connection == ConnectionClass.Unmetered,
                    )
                }.toImmutableList(),
        fetches = fetches.map { it.copy(isDegraded = degraded) }.toImmutableList(),
        scope = SyncScope.MetadataThumbnailsAndOriginal,
    )

    private fun entry(
        tier: UploadTier,
        progress: TierProgress,
        failure: ErrorCode? = null,
    ) = UploadQueueEntry(
        id = AssetId("upload-${tier.wire}"),
        capturedAt = CAPTURED_AT,
        tier = tier,
        progress = progress,
        failure = failure,
        pinsLocalOriginal = true,
    )

    private fun fetch() =
        FetchQueueEntry(
            id = AssetId("fetch"),
            capturedAt = CAPTURED_AT,
            wanted = RepresentationTier.Original,
            held = RepresentationTier.Thumbnail,
        )

    private companion object {
        val CAPTURED_AT: Instant = Instant.parse("2026-06-01T12:00:00Z")
    }
}
