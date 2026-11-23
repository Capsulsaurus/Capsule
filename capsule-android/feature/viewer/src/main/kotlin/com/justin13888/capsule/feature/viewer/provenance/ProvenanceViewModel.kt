package com.justin13888.capsule.feature.viewer.provenance

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.feature.viewer.ViewerFailure
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/** One link of the chain, with the device that signed it named where the directory can. */
@Immutable
data class ProvenanceEntry(
    /** The signed record. */
    val record: ProvenanceRecord,
    /** The signing device's label, or `null` when the directory has none for it. */
    val deviceLabel: String?,
)

/** What the provenance screen is showing. */
@Immutable
sealed interface ProvenanceUiState {
    /** The chain has not been read yet. */
    data object Loading : ProvenanceUiState

    /**
     * The chain is empty.
     *
     * Reachable only for an asset this device holds no history for at all. Every asset has a
     * `create`, so an empty chain means "not synced here yet", never "never happened".
     */
    data object Empty : ProvenanceUiState

    /** The read failed with a code the user can act on. */
    data class Failed(
        val failure: ViewerFailure,
    ) : ProvenanceUiState

    /** The chain, oldest first. */
    data class Ready(
        val entries: ImmutableList<ProvenanceEntry>,
    ) : ProvenanceUiState
}

/**
 * The asset's append-only chain, with device labels folded in.
 *
 * The chain is presented oldest-first and complete — a delete record is never dropped once
 * the asset is restored, because "deleted on X, restored on Y" is the history the user came
 * for and a current-state summary would erase exactly the part worth knowing.
 *
 * Device names are combined in rather than joined per row so that an unreachable directory
 * degrades one column instead of failing the screen: a chain whose devices are unnamed is
 * still a chain.
 */
class ProvenanceViewModel(
    private val assetId: AssetId,
    private val assets: AssetRepository,
    private val sessions: SessionRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow<ProvenanceUiState>(ProvenanceUiState.Loading)

    /** The one immutable value the screen renders. */
    val state: StateFlow<ProvenanceUiState> = mutableState.asStateFlow()

    init {
        observe()
    }

    /** Re-reads the chain — the remediation behind the failure state. */
    fun retry() {
        mutableState.value = ProvenanceUiState.Loading
        observe()
    }

    private fun observe() {
        viewModelScope.launch {
            combine(
                assets.provenance(assetId),
                sessions.devices().catch { emit(emptyList()) },
            ) { records, devices -> toState(records, devices) }
                .catch { cause ->
                    if (cause is CapsuleError) emit(ProvenanceUiState.Failed(ViewerFailure.of(cause))) else throw cause
                }.collect { mutableState.value = it }
        }
    }

    private fun toState(
        records: List<ProvenanceRecord>,
        devices: List<Device>,
    ): ProvenanceUiState {
        if (records.isEmpty()) return ProvenanceUiState.Empty
        val labels = devices.mapNotNull { device -> device.displayName?.let { device.id.value to it } }.toMap()
        return ProvenanceUiState.Ready(
            records
                .sortedBy(ProvenanceRecord::at)
                .map { ProvenanceEntry(it, labels[it.byDevice.value]) }
                .toImmutableList(),
        )
    }
}
