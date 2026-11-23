package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performScrollTo
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.feature.viewer.editor.CAPTION_FIELD_TAG
import com.justin13888.capsule.feature.viewer.editor.EDITING_DISABLED_TAG
import com.justin13888.capsule.feature.viewer.editor.EDITOR_REFUSAL_TAG
import com.justin13888.capsule.feature.viewer.editor.METADATA_EDITOR_TAG
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorActions
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorScreen
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorState
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorUiState
import com.justin13888.capsule.feature.viewer.editor.SUPERSEDED_TAG
import com.justin13888.capsule.feature.viewer.info.ASSET_INFO_TAG
import com.justin13888.capsule.feature.viewer.info.AssetInfo
import com.justin13888.capsule.feature.viewer.info.AssetInfoPanel
import com.justin13888.capsule.feature.viewer.info.AssetInfoUiState
import com.justin13888.capsule.feature.viewer.info.SYNC_ROW_TAG
import com.justin13888.capsule.feature.viewer.info.TierHolding
import com.justin13888.capsule.feature.viewer.info.tierRowTag
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

/**
 * The two surfaces behind the photo: what is known about it, and what may be changed.
 *
 * The panel is checked for saying the absent things out loud — every rung listed, held or
 * not — and the editor for the one refusal that has to be visible rather than silent: an
 * asset a newer peer wrote is shown with its controls disabled and a banner explaining why,
 * never with the controls quietly removed.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AssetDetailScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `the panel lists every rung of the ladder`() {
        renderPanel(mutableStateOf(infoState()))

        RepresentationTier.entries.forEach { tier ->
            compose.onNodeWithTag(tierRowTag(TierHolding(tier, isHeld = false))).performScrollTo().assertIsDisplayed()
        }
    }

    @Test
    fun `the panel says where the asset stands relative to the server`() {
        renderPanel(mutableStateOf(infoState(syncState = SyncState.AwaitingOriginal)))

        compose.onNodeWithTag(SYNC_ROW_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_info_sync_awaiting_original)).assertIsDisplayed()
    }

    @Test
    fun `the panel discloses the export strip beside the camera serial`() {
        renderPanel(mutableStateOf(infoState()))

        compose.onNodeWithTag(ASSET_INFO_TAG).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_info_privacy_note)).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `an asset that is gone reads as empty in the panel too`() {
        renderPanel(mutableStateOf(AssetInfoUiState.Missing))

        compose.onNodeWithText(string(R.string.viewer_empty_title)).assertIsDisplayed()
    }

    @Test
    fun `the editor disables its controls and says why for an asset from a newer peer`() {
        val forward = landmark(ViewerFixture.LANDMARKS.forwardSchemaAssetId)
        renderEditor(mutableStateOf(editorState(forward)))

        compose.onNodeWithTag(EDITING_DISABLED_TAG).assertIsDisplayed()
        compose.onNodeWithTag(CAPTION_FIELD_TAG).assertIsNotEnabled()
    }

    @Test
    fun `an ordinary asset gets an editable caption and no disabled banner`() {
        renderEditor(mutableStateOf(editorState(ViewerFixture.ordinaryAsset())))

        compose.onNodeWithTag(METADATA_EDITOR_TAG).assertIsDisplayed()
        compose.onNodeWithTag(EDITING_DISABLED_TAG).assertDoesNotExist()
    }

    @Test
    fun `a displaced caption is offered back rather than lost`() {
        val captioned =
            ViewerFixture.LIBRARY.captions.entries
                .first { it.value.superseded.isNotEmpty() }
        val asset = requireNotNull(ViewerFixture.LIBRARY.assetsById[captioned.key])
        renderEditor(mutableStateOf(editorState(asset, caption = captioned.value)))

        compose.onNodeWithTag(SUPERSEDED_TAG).assertIsDisplayed()
        compose
            .onNodeWithText(
                captioned.value.superseded
                    .first()
                    .value,
            ).assertIsDisplayed()
    }

    @Test
    fun `a refused write is reported with its localized code`() {
        val refusal = ViewerFailure(code = "error.quota.grace_locked", detail = "grace expired")
        renderEditor(mutableStateOf(editorState(ViewerFixture.ordinaryAsset(), refusal = refusal)))

        compose.onNodeWithTag(EDITOR_REFUSAL_TAG).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.error_quota_grace_locked)).assertIsDisplayed()
    }

    private fun renderPanel(state: MutableState<AssetInfoUiState>) {
        compose.setContent { CapsulePreviewSurface { AssetInfoPanel(state = state.value, onRetry = {}) } }
        compose.waitForIdle()
    }

    private fun renderEditor(state: MutableState<MetadataEditorUiState>) {
        compose.setContent { CapsulePreviewSurface { MetadataEditorScreen(state = state.value, actions = noop()) } }
        compose.waitForIdle()
    }

    private fun infoState(
        asset: Asset = ViewerFixture.ordinaryAsset(),
        syncState: SyncState = SyncState.Synced,
    ): AssetInfoUiState =
        AssetInfoUiState.Ready(
            AssetInfo.of(
                asset = asset,
                held = asset.heldRepresentations,
                syncState = syncState,
                albumName = null,
            ),
        )

    private fun editorState(
        asset: Asset,
        caption: Caption = Caption(current = null),
        refusal: ViewerFailure? = null,
    ): MetadataEditorUiState = MetadataEditorUiState.Ready(MetadataEditorState.of(asset, caption, AssetTags(), refusal))

    private fun landmark(id: com.justin13888.capsule.core.model.AssetId?): Asset =
        requireNotNull(ViewerFixture.LIBRARY.assetsById[requireNotNull(id)])

    private fun string(key: Int): String = RuntimeEnvironment.getApplication().getString(key)

    @Suppress("LongParameterList")
    private fun noop(): MetadataEditorActions =
        MetadataEditorActions(
            onSetCaption = {},
            onClearCaption = {},
            onRestoreCaption = {},
            onSetRating = {},
            onSetCull = {},
            onSetHidden = {},
            onAddUserTag = {},
            onRemoveUserTag = {},
            onPromoteAiTag = {},
            onDismissAiTag = {},
            onConfirmLocation = {},
            onClearLocation = {},
            onDismissRefusal = {},
            onRetry = {},
        )
}
