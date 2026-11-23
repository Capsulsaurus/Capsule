package com.justin13888.capsule.feature.albums.ui.places

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.data.PlaceClustering
import com.justin13888.capsule.feature.albums.model.MapBounds
import com.justin13888.capsule.feature.albums.model.PlaceCluster
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** What the Places view is showing. */
sealed interface PlacesUiState {
    /** Sampling the library and clustering its fixes. */
    data object Loading : PlacesUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : PlacesUiState

    /** The pins, the window they fit in, and whichever one is selected. */
    data class Content(
        val clusters: ImmutableList<PlaceCluster> = persistentListOf(),
        /** The geographic window the map draws, or `null` when there is nothing to draw. */
        val bounds: MapBounds? = null,
        /** The pin the user tapped. */
        val selected: PlaceCluster? = null,
        /** The selected pin's photographs. */
        val selectedAssets: ImmutableList<Asset> = persistentListOf(),
        /**
         * Whether any pin's fixes are stored in a second datum.
         *
         * Surfaced because those pins are drawn a few hundred metres from where a WGS-84 map
         * would put them: coordinates are stored verbatim in the datum their source supplied
         * and are never converted at rest, so the honest answer is to say the plot is
         * approximate rather than to convert and destroy the ground truth.
         */
        val hasSecondDatum: Boolean = false,
    ) : PlacesUiState
}

/**
 * The library laid out by where it was taken.
 *
 * The map itself is drawn procedurally rather than fetched, which is a privacy property
 * before it is a dependency choice: a tile request tells whoever serves the tiles exactly
 * where the user's photographs were taken, which is precisely the fingerprinting surface
 * the rest of the system works to keep off the wire.
 */
class PlacesViewModel(
    private val sample: LibrarySample,
) : ViewModel() {
    private val state = MutableStateFlow<PlacesUiState>(PlacesUiState.Loading)

    /** What the view is showing right now. */
    val uiState: StateFlow<PlacesUiState> = state.asStateFlow()

    private var sampled: List<Asset> = emptyList()

    init {
        load()
    }

    /** Re-reads the sample and re-clusters it. */
    fun load() {
        viewModelScope.launch {
            state.value =
                try {
                    sampled = sample.assets().assets
                    val clusters = PlaceClustering.cluster(sampled)
                    PlacesUiState.Content(
                        clusters = clusters.toImmutableList(),
                        bounds = MapBounds.around(clusters),
                        hasSecondDatum = clusters.any { it.datum != GpsDatum.Wgs84 },
                    )
                } catch (failure: CapsuleError) {
                    PlacesUiState.Failed(failure.code?.value)
                }
        }
    }

    /** Selects a pin, loading the photographs behind it from the sample already in hand. */
    fun select(cluster: PlaceCluster?) {
        val current = state.value as? PlacesUiState.Content ?: return
        val ids = cluster?.assetIds?.toSet().orEmpty()
        state.value =
            current.copy(
                selected = cluster,
                selectedAssets = sampled.filter { it.id in ids }.toImmutableList(),
            )
    }
}
