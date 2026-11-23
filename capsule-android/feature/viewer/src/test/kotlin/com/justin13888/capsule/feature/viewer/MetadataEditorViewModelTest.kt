package com.justin13888.capsule.feature.viewer

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorUiState
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The editor's writes, through the same repositories the app uses.
 *
 * Two properties are worth more than the rest. A caption write must *displace* rather than
 * overwrite, because a plain last-writer-wins register silently loses one side of a concurrent
 * edit and that is real data loss. And a write to an asset a newer peer produced must be
 * refused with a code the user can read, because this build cannot re-sign a sidecar whose
 * tokens it does not understand — hiding that as a control that quietly does nothing would be
 * indistinguishable from a bug.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MetadataEditorViewModelTest {
    private val landmarks = ViewerFixture.LANDMARKS

    @BeforeEach
    fun installTestDispatcher() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @AfterEach
    fun removeTestDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `writing a caption displaces the previous value into the superseded log`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val captioned =
                ViewerFixture.LIBRARY.captions.keys
                    .first()
            val previous =
                ViewerFixture.LIBRARY.captions
                    .getValue(captioned)
                    .current
                    ?.value
            val viewModel = editor(captioned, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.setCaption("a new caption")
                val ready = viewModel.state.value as MetadataEditorUiState.Ready

                assertEquals(
                    "a new caption",
                    ready.editor.caption.current
                        ?.value,
                )
                assertTrue(
                    ready.editor.caption.superseded
                        .any { it.value == previous },
                ) {
                    "the displaced caption must remain recoverable"
                }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `restoring a displaced caption writes it again as the newest value`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val captioned =
                ViewerFixture.LIBRARY.captions.entries
                    .first { it.value.superseded.isNotEmpty() }
            val displaced = captioned.value.superseded.first()
            val viewModel = editor(captioned.key, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.restoreCaption(displaced)
                val ready = viewModel.state.value as MetadataEditorUiState.Ready

                assertEquals(
                    displaced.value,
                    ready.editor.caption.current
                        ?.value,
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `promoting a suggestion copies it into the user set and clears the suggestion`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val tagged =
                ViewerFixture.LIBRARY.tags.entries
                    .first { it.value.ai.isNotEmpty() }
            val suggestion = tagged.value.ai.first()
            val viewModel = editor(tagged.key, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.promoteAiTag(suggestion.addId)
                val tags = (viewModel.state.value as MetadataEditorUiState.Ready).editor.tags

                assertTrue(tags.user.any { it.tag == suggestion.tag })
                assertFalse(tags.ai.any { it.addId == suggestion.addId })
                // A fresh user-scoped add id, never the model's: promotion is a statement
                // about the user's intent and the model's entry is not the user's to reuse.
                assertFalse(tags.user.any { it.addId == suggestion.addId })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `dismissing a suggestion removes only that add`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val tagged =
                ViewerFixture.LIBRARY.tags.entries
                    .first { it.value.ai.isNotEmpty() }
            val suggestion = tagged.value.ai.first()
            val viewModel = editor(tagged.key, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.dismissAiTag(suggestion.addId)
                val tags = (viewModel.state.value as MetadataEditorUiState.Ready).editor.tags

                assertFalse(tags.ai.any { it.addId == suggestion.addId })
                assertFalse(tags.user.any { it.tag == suggestion.tag })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the rating and the cull flag move independently of each other`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val asset = ViewerFixture.ordinaryAsset()
            val viewModel = editor(asset.id, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.setRating(3)
                viewModel.setCull(CullFlag.Reject)
                val editor = (viewModel.state.value as MetadataEditorUiState.Ready).editor

                assertEquals(3, editor.asset.rating)
                assertEquals(CullFlag.Reject, editor.asset.cull.knownOrNull)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `hiding is view-layer only and leaves the container album alone`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val asset = ViewerFixture.ordinaryAsset()
            val viewModel = editor(asset.id, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.setHidden(true)
                val hidden = (viewModel.state.value as MetadataEditorUiState.Ready).editor.asset

                assertTrue(hidden.isHidden)
                assertEquals(asset.albumId, hidden.albumId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `confirming an inferred fix changes its source and not its coordinates`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val inferred =
                ViewerFixture.LIBRARY.assets.firstOrNull { it.gps?.source?.knownOrNull == GpsSource.Inferred }
            if (inferred == null) return@runTest
            val viewModel = editor(inferred.id, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.confirmLocation()
                val fix = (viewModel.state.value as MetadataEditorUiState.Ready).editor.asset.gps

                assertEquals(GpsSource.Manual, fix?.source?.knownOrNull)
                assertEquals(inferred.gps?.latitude, fix?.latitude)
                assertEquals(inferred.gps?.datum?.raw, fix?.datum?.raw)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an asset from a newer peer refuses every write with a code the user can read`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.forwardSchemaAssetId, "forward-schema")
            val viewModel = editor(id, repositories)

            viewModel.state.test {
                val ready = awaitFirst<MetadataEditorUiState.Ready>()
                assertFalse(ready.editor.isEditable)

                viewModel.setRating(4)
                val refused = viewModel.state.value as MetadataEditorUiState.Ready

                assertEquals("error.protocol.version_unsupported", refused.editor.refusal?.code)
                assertEquals(ready.editor.asset.rating, refused.editor.asset.rating)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a refusal can be acknowledged so its banner leaves`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.forwardSchemaAssetId, "forward-schema")
            val viewModel = editor(id, repositories)

            viewModel.state.test {
                awaitFirst<MetadataEditorUiState.Ready>()
                viewModel.setRating(4)
                viewModel.dismissRefusal()

                val cleared = viewModel.state.value as MetadataEditorUiState.Ready
                assertEquals(null, cleared.editor.refusal)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun editor(
        id: AssetId,
        repositories: MockRepositories,
    ) = MetadataEditorViewModel(assetId = id, assets = repositories.assets, metadata = repositories.metadata)
}
