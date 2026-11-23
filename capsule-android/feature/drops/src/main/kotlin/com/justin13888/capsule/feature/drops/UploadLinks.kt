package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.UploadLink
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Instant

/**
 * One upload link as a list renders it.
 *
 * [acceptsDeposits] is computed once here rather than recomputed while drawing, so the row's
 * badge, its supporting line and the detail screen behind it cannot disagree about whether
 * the link is still open — the sort of split-brain a `Clock.instant()` call inside a
 * composable produces the moment a recomposition straddles the expiry.
 */
@Immutable
data class UploadLinkRow(
    /** The link itself, caps and usage included. */
    val link: UploadLink,
    /** Whether the link would accept a deposit at the moment of the reading. */
    val acceptsDeposits: Boolean,
)

/**
 * Everything the upload-link list draws, as one value.
 */
@Immutable
data class UploadLinksUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** Why the reading failed, or `null`. */
    val failure: DropsFailure? = null,
    /** Every provisioned link, newest first. */
    val links: List<UploadLinkRow> = emptyList(),
) {
    /** Nothing has been provisioned. */
    val isEmpty: Boolean get() = !isLoading && failure == null && links.isEmpty()

    /** Whether the failure still leaves usable content on screen. */
    val isDegraded: Boolean get() = failure != null && links.isNotEmpty()
}

/**
 * The upload-link list's state machine.
 *
 * [clock] is a constructor default rather than an injected binding: liveness is a statement
 * about wall-clock time, and a test that needs to sit either side of an expiry passes its own
 * fixed clock instead of the app graph carrying one nothing else wants.
 */
class UploadLinksViewModel(
    private val sharing: SharingRepository,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(UploadLinksUiState())

    /** Everything the list draws. */
    val state: StateFlow<UploadLinksUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Restarts the reading. Also the remediation behind the error state's action. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                sharing
                    .uploadLinks()
                    .catch { throwable ->
                        val failure = DropsFailure.of(throwable)
                        mutableState.update { it.copy(isLoading = false, failure = failure) }
                    }.collect { links -> mutableState.update { it.loaded(links, clock.instant()) } }
            }
    }

    private fun UploadLinksUiState.loaded(
        links: List<UploadLink>,
        now: Instant,
    ): UploadLinksUiState =
        copy(
            isLoading = false,
            failure = null,
            links =
                links
                    .sortedByDescending { it.createdAt }
                    .map { UploadLinkRow(it, it.acceptsDepositsAt(now)) },
        )
}
