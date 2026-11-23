package com.justin13888.capsule.feature.viewer.info

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.StorageRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.feature.viewer.ViewerFailure
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch

/**
 * Assembles the information panel from the four contracts that own its facts.
 *
 * Combined rather than read once, because three of the four move under the user: a tier
 * arrives, an upload finishes, a caption is edited beside the panel. A panel that snapshotted
 * on open would show a fetch as missing seconds after it landed, which is exactly the moment
 * someone is looking to see whether it landed.
 *
 * The album name comes last and through a `flatMapLatest` because it depends on the asset —
 * an asset moved between albums has to re-point the read rather than keep reporting the album
 * it used to be in.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AssetInfoViewModel(
    private val assetId: AssetId,
    private val assets: AssetRepository,
    private val storage: StorageRepository,
    private val sync: SyncRepository,
    private val albums: AlbumRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow<AssetInfoUiState>(AssetInfoUiState.Loading)

    /** The one immutable value the panel renders. */
    val state: StateFlow<AssetInfoUiState> = mutableState.asStateFlow()

    init {
        observe()
    }

    /** Restarts the reads — the remediation behind the failure state. */
    fun retry() {
        mutableState.value = AssetInfoUiState.Loading
        observe()
    }

    private fun observe() {
        viewModelScope.launch {
            combine(
                assets.observeAsset(assetId),
                storage.heldRepresentations(assetId),
                sync.assetSyncState(assetId),
                albumNames(),
            ) { asset, held, syncState, albumsByName ->
                toState(asset, held, syncState, albumsByName)
            }.catch { cause ->
                // Only a coded failure becomes a visible state. Anything else is a bug in a
                // fake or an adapter, and swallowing it into a banner would hide it.
                if (cause is CapsuleError) emit(AssetInfoUiState.Failed(ViewerFailure.of(cause))) else throw cause
            }.collect { mutableState.value = it }
        }
    }

    /**
     * Every album this asset could be in, keyed by id.
     *
     * Read through the asset so the panel re-points when the asset moves, and tolerant of a
     * missing album: an asset filed into a container this device has not synced yet is still
     * an asset worth describing.
     */
    private fun albumNames() =
        assets.observeAsset(assetId).flatMapLatest { asset ->
            asset?.albumId?.let { albums.album(it) } ?: flowOf<Album?>(null)
        }

    private fun toState(
        asset: Asset?,
        held: HeldRepresentations,
        syncState: SyncState,
        album: Album?,
    ): AssetInfoUiState =
        if (asset == null) {
            AssetInfoUiState.Missing
        } else {
            AssetInfoUiState.Ready(AssetInfo.of(asset, held, syncState, album?.name))
        }
}
