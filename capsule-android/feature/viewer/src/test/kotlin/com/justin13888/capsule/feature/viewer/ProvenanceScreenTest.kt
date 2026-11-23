package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.feature.viewer.provenance.PROVENANCE_TAG
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceEntry
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceScreen
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceUiState
import kotlinx.collections.immutable.toImmutableList
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.time.Instant

/**
 * The chain, as the user reads it.
 *
 * Two claims are checked. A `create` says so rather than leaving a blank line where the prior
 * hash would go — a blank reads as missing data instead of as the beginning of a chain. And
 * an action outside this build's closed set is rendered with its raw token rather than
 * dropped: the set is closed and an unknown value is a structural error, but it is also
 * someone's history, and a gap in an append-only chain is worse than an unfamiliar word.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ProvenanceScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val assetId = AssetId("00000000-0000-7000-8000-00000000000a")

    @Test
    fun `an empty chain reads as nothing recorded`() {
        render(mutableStateOf(ProvenanceUiState.Empty))

        compose.onNodeWithText(string(R.string.viewer_provenance_empty)).assertIsDisplayed()
    }

    @Test
    fun `the first record says it begins the chain`() {
        render(mutableStateOf(ready(listOf(create()))))

        compose.onNodeWithTag(PROVENANCE_TAG).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_provenance_root)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_provenance_action_create)).assertIsDisplayed()
    }

    @Test
    fun `a delete stays legible beside the create it followed`() {
        render(mutableStateOf(ready(listOf(create(), delete()))))

        compose.onNodeWithText(string(R.string.viewer_provenance_action_create)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_provenance_action_delete)).assertIsDisplayed()
    }

    @Test
    fun `an action this build does not know keeps its raw token rather than vanishing`() {
        val unknown = create().copy(action = Wire.Unknown("resurrect"), recordHash = "cafe")

        render(mutableStateOf(ready(listOf(unknown))))

        compose.onNodeWithTag(PROVENANCE_TAG).assertIsDisplayed()
        compose
            .onNodeWithText(string(R.string.viewer_provenance_action_unknown).withArguments("token" to "resurrect"))
            .assertIsDisplayed()
    }

    private fun render(state: MutableState<ProvenanceUiState>) {
        compose.setContent { CapsulePreviewSurface { ProvenanceScreen(state = state.value, onRetry = {}) } }
        compose.waitForIdle()
    }

    private fun ready(records: List<ProvenanceRecord>) =
        ProvenanceUiState.Ready(records.map { ProvenanceEntry(it, deviceLabel = null) }.toImmutableList())

    private fun create() =
        ProvenanceRecord(
            assetId = assetId,
            action = LifecycleAction.Create.asWire(),
            priorProvenanceHash = null,
            recordHash = "aaaa1111",
            byDevice = ViewerFixture.LANDMARKS.thisDeviceId,
            at = Instant.parse("2026-01-01T10:00:00Z"),
        )

    private fun delete() =
        ProvenanceRecord(
            assetId = assetId,
            action = LifecycleAction.Delete.asWire(),
            priorProvenanceHash = "aaaa1111",
            recordHash = "bbbb2222",
            byDevice = ViewerFixture.LANDMARKS.thisDeviceId,
            at = Instant.parse("2026-02-01T10:00:00Z"),
            retentionUntil = Instant.parse("2026-03-03T10:00:00Z"),
        )

    private fun string(key: Int): String = RuntimeEnvironment.getApplication().getString(key)
}
