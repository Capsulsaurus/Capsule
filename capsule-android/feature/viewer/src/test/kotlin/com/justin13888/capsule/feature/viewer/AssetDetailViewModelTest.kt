package com.justin13888.capsule.feature.viewer

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.viewer.info.AssetInfoUiState
import com.justin13888.capsule.feature.viewer.info.AssetInfoViewModel
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceUiState
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceViewModel
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
 * The two read-only surfaces behind the viewer: what is known, and what happened.
 *
 * The information panel's job is to be honest about *absence* — which rungs are not held,
 * that a location was never recorded — and the provenance screen's is to be honest about
 * history, including the parts a summary would erase. A delete record survives a restore, so
 * "deleted on X, restored on Y" stays legible forever; a chain that dropped it would answer
 * the only question the screen exists for by silence.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AssetDetailViewModelTest {
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
    fun `the panel lists every rung of the ladder, held or not`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.degradedAssetId, "degraded")
            val viewModel =
                AssetInfoViewModel(
                    assetId = id,
                    assets = repositories.assets,
                    storage = repositories.storage,
                    sync = repositories.sync,
                    albums = repositories.albums,
                )

            viewModel.state.test {
                val info = awaitFirst<AssetInfoUiState.Ready>().info

                assertEquals(RepresentationTier.entries.size, info.tiers.size)
                assertTrue(info.tiers.single { it.tier == RepresentationTier.Lqip }.isHeld)
                assertFalse(info.tiers.single { it.tier == RepresentationTier.Original }.isHeld) {
                    "the panel must say plainly that the original is not here"
                }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the panel reports the awaiting-original state as pending rather than as absent`() =
        runTest {
            val repositories = ViewerFixture.repositories(MockScenario.AwaitingOriginal)
            val id = ViewerFixture.require(landmarks.awaitingOriginalAssetId, "awaiting-original")
            val viewModel =
                AssetInfoViewModel(
                    assetId = id,
                    assets = repositories.assets,
                    storage = repositories.storage,
                    sync = repositories.sync,
                    albums = repositories.albums,
                )

            viewModel.state.test {
                val info = awaitFirst<AssetInfoUiState.Ready>().info

                assertEquals(SyncState.AwaitingOriginal, info.syncState)
                assertTrue(ViewerDegradation.OriginalOnAnotherDevice in info.degradations)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the panel keeps the datum that came with the coordinates`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.gcj02AssetId, "gcj02")
            val stored = requireNotNull(ViewerFixture.LIBRARY.assetsById[id]?.gps)
            val viewModel =
                AssetInfoViewModel(
                    assetId = id,
                    assets = repositories.assets,
                    storage = repositories.storage,
                    sync = repositories.sync,
                    albums = repositories.albums,
                )

            viewModel.state.test {
                val fix = requireNotNull(awaitFirst<AssetInfoUiState.Ready>().info.asset.gps)

                // Stored verbatim in the datum the source supplied, never converted at rest.
                assertEquals(stored.datum.raw, fix.datum.raw)
                assertEquals(stored.latitude, fix.latitude)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a delete record survives beside its create, oldest first`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.trashedMidWindowAssetId, "trashed")
            val viewModel =
                ProvenanceViewModel(assetId = id, assets = repositories.assets, sessions = repositories.sessions)

            viewModel.state.test {
                val entries = awaitFirst<ProvenanceUiState.Ready>().entries
                val actions = entries.map { it.record.action.knownOrNull }

                assertEquals(listOf(LifecycleAction.Create, LifecycleAction.Delete), actions)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the create record is the only link with no prior hash`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.trashedMidWindowAssetId, "trashed")
            val viewModel =
                ProvenanceViewModel(assetId = id, assets = repositories.assets, sessions = repositories.sessions)

            viewModel.state.test {
                val entries = awaitFirst<ProvenanceUiState.Ready>().entries

                assertEquals(null, entries.first().record.priorProvenanceHash)
                assertEquals(entries.first().record.recordHash, entries[1].record.priorProvenanceHash)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a delete record carries the retention floor signed into it`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val id = ViewerFixture.require(landmarks.trashedMidWindowAssetId, "trashed")
            val viewModel =
                ProvenanceViewModel(assetId = id, assets = repositories.assets, sessions = repositories.sessions)

            viewModel.state.test {
                val entries = awaitFirst<ProvenanceUiState.Ready>().entries
                val delete = entries.single { it.record.action.knownOrNull == LifecycleAction.Delete }

                assertTrue(delete.record.retentionUntil != null) {
                    "the purge floor travels with the deletion, not with server config"
                }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an untouched asset still shows the create every asset has`() =
        runTest {
            val repositories = ViewerFixture.repositories()
            val asset = ViewerFixture.ordinaryAsset()
            val viewModel =
                ProvenanceViewModel(assetId = asset.id, assets = repositories.assets, sessions = repositories.sessions)

            viewModel.state.test {
                val entries = awaitFirst<ProvenanceUiState.Ready>().entries

                assertEquals(1, entries.size)
                assertEquals(
                    LifecycleAction.Create,
                    entries
                        .single()
                        .record.action.knownOrNull,
                )
                cancelAndIgnoreRemainingEvents()
            }
        }
}
