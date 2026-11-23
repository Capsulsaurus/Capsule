package com.justin13888.capsule.feature.albums.ui.hidden

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.data.HiddenAssetSource
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** What the Hidden view is showing. */
sealed interface HiddenUiState {
    /** Reading the window. */
    data object Loading : HiddenUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : HiddenUiState

    /** The assets the user hid. */
    data class Content(
        val assets: ImmutableList<Asset> = persistentListOf(),
    ) : HiddenUiState
}

/**
 * The Hidden view, behind the shell's local-auth gate.
 *
 * Nothing here re-checks the gate, and that is deliberate rather than an omission: the gate
 * is applied by the routing table from the destination's own flag, so this screen is not
 * composed at all until auth has succeeded. A second check inside the feature would be a
 * second thing to forget.
 *
 * Unhiding is one stamped write of a last-writer-wins register. It is view-layer only — the
 * asset never left its album, never stopped syncing, and was always reachable from its stack
 * or an existing share.
 */
class HiddenViewModel(
    private val source: HiddenAssetSource,
    private val assets: AssetRepository,
) : ViewModel() {
    private val state = MutableStateFlow<HiddenUiState>(HiddenUiState.Loading)

    /** What the view is showing right now. */
    val uiState: StateFlow<HiddenUiState> = state.asStateFlow()

    init {
        load()
    }

    /** Re-reads the window. */
    fun load() {
        viewModelScope.launch {
            state.value =
                try {
                    HiddenUiState.Content(source.hidden(0, PAGE_SIZE).toImmutableList())
                } catch (failure: CapsuleError) {
                    HiddenUiState.Failed(failure.code?.value)
                }
        }
    }

    /** Clears an asset's hidden flag, returning it to every default view. */
    fun unhide(id: AssetId) {
        viewModelScope.launch {
            runCatching { assets.setHidden(id, hidden = false) }
            load()
        }
    }

    private companion object {
        /** How many hidden assets are read at once. */
        const val PAGE_SIZE = 256
    }
}
