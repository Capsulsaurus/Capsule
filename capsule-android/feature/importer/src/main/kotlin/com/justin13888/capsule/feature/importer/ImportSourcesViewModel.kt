package com.justin13888.capsule.feature.importer

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.collections.immutable.toImmutableList
import kotlinx.collections.immutable.toImmutableSet
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The source picker's state machine.
 *
 * The scope-mapping prompt is enforced here rather than on the screen, because "ask once"
 * is a property of the *import*, not of one composition: a selection that includes a source
 * Capsule has never imported from cannot be scanned until that source has an answer, and a
 * screen that merely happened to show a dialog would let a rotation skip it.
 */
class ImportSourcesViewModel(
    private val imports: ImportRepository,
    private val albums: AlbumRepository,
    private val scanner: SourceScanner,
    private val scopeMappings: ScopeMappingStore,
    private val plans: ImportPlanStore,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ImportSourcesUiState())
    private val adHoc = MutableStateFlow<List<ImportSource>>(emptyList())

    /** Everything the picker draws. */
    val state: StateFlow<ImportSourcesUiState> = mutableState.asStateFlow()

    private var collection: Job? = null
    private var scanJob: Job? = null

    init {
        reload()
    }

    /** Restarts the read of sources and albums. Also the error state's remediation. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null, hasLocalFailure = false) }
        collection =
            viewModelScope.launch {
                combine(imports.scopes(), albums.albums(), scopeMappings.mappings(), adHoc) { scopes, all, map, extra ->
                    val known = scopes.map { ImportSource(scope = it, destination = map[it.locator]) }
                    val unknown =
                        extra
                            .filter { source -> known.none { it.locator == source.locator } }
                            .map { it.copy(destination = map[it.locator]) }
                    val choices =
                        all
                            .filter { it.kind.knownOrNull == AlbumKind.Container }
                            .map { ImportAlbumChoice(it.id, it.name, it.isDefault) }
                    (known + unknown) to choices
                }.catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { (sources, choices) ->
                        mutableState.update { current ->
                            current.copy(
                                isLoading = false,
                                failure = null,
                                hasLocalFailure = false,
                                sources = sources.toImmutableList(),
                                albums = choices.toImmutableList(),
                            )
                        }
                    }
            }
    }

    /** Ticks or unticks one source. */
    fun toggle(locator: String) {
        mutableState.update { current ->
            val next = if (locator in current.selected) current.selected - locator else current.selected + locator
            current.copy(selected = next.toImmutableSet())
        }
    }

    /**
     * Adds a source the user pointed at by hand — a picked folder, an archive, a camera.
     *
     * Kept beside the discovered ones rather than in a separate list, so the destination
     * question and the scan behave identically however the source was found.
     */
    fun addSource(
        locator: String,
        kind: SourceKind,
    ) {
        adHoc.update { current ->
            if (current.any { it.locator == locator }) current else current + ImportSource.adHoc(locator, kind)
        }
        mutableState.update { it.copy(selected = (it.selected + locator).toImmutableSet()) }
    }

    /**
     * Starts a scan, or raises the destination question first.
     *
     * The order is the point: a source with no remembered answer stops the scan rather than
     * being filed somewhere Capsule chose on the user's behalf.
     */
    fun scan() {
        val current = mutableState.value
        if (!current.canScan) return
        val unmapped = current.unmappedSelection
        if (unmapped != null) {
            mutableState.update { it.copy(prompt = ScopeMappingPrompt(unmapped, it.albums.firstOrNull()?.id)) }
            return
        }
        scanJob?.cancel()
        scanJob = viewModelScope.launch { runScan(current.selectedSources) }
    }

    /** Highlights an album inside the destination question without committing to it. */
    fun choosePromptAlbum(albumId: AlbumId) {
        mutableState.update { current -> current.copy(prompt = current.prompt?.copy(chosen = albumId)) }
    }

    /** Puts the destination question away without answering it. The scan does not start. */
    fun dismissPrompt() {
        mutableState.update { it.copy(prompt = null) }
    }

    /**
     * Answers the destination question, for good.
     *
     * Written through the repository — the durable record — and mirrored into the local store
     * so the picker can tell it has already asked. Both, because the contract offers a write
     * and no matching read.
     */
    fun confirmPrompt() {
        val prompt = mutableState.value.prompt ?: return
        val albumId = prompt.chosen ?: return
        viewModelScope.launch {
            runCatching { imports.setScopeDestination(prompt.source.scope, albumId) }
                .onSuccess {
                    scopeMappings.remember(prompt.source.scope, albumId)
                    mutableState.update { it.answered(prompt.source.locator, albumId) }
                    scan()
                }.onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /** Forgets the plan this picker produced, so leaving and returning starts clean. */
    fun clearPlan() {
        mutableState.update { it.copy(plannedImportId = null) }
    }

    private suspend fun runScan(sources: List<ImportSource>) {
        val locators = mutableListOf<String>()
        sources.forEach { source ->
            mutableState.update { it.copy(scan = ScanProgress(source.locator, locators.size)) }
            locators += scanner.scan(source)
            mutableState.update { it.copy(scan = ScanProgress(source.locator, locators.size)) }
        }
        val destination = singleDestination(sources)
        runCatching { imports.plan(locators, destination) }
            .onSuccess { plan ->
                plans.put(PlannedImport(plan, locators, destination))
                mutableState.update { it.copy(scan = null, plannedImportId = plan.importId) }
            }.onFailure { throwable -> mutableState.update { it.copy(scan = null).failed(throwable) } }
    }

    /**
     * The album every selected source agrees on, or `null` when they disagree.
     *
     * A plan carries one destination, so a mixed selection hands the decision back to the
     * default-album rules — which record which rule fired, keeping a surprising destination
     * explainable rather than arbitrary.
     */
    private fun singleDestination(sources: List<ImportSource>): AlbumId? =
        sources.mapNotNull(ImportSource::destination).distinct().singleOrNull()

    /**
     * Files the answer into the row that asked, without waiting for the mapping flow.
     *
     * The write has already landed, so the flow will converge on the same value — but it has
     * not arrived yet, and the scan that follows reads this state. Without the patch the picker
     * would look at a source it has just been given a destination for, decide it is still
     * unmapped, and ask the question a second time.
     */
    private fun ImportSourcesUiState.answered(
        locator: String,
        albumId: AlbumId,
    ): ImportSourcesUiState =
        copy(
            prompt = null,
            sources =
                sources
                    .map { source -> if (source.locator == locator) source.copy(destination = albumId) else source }
                    .toImmutableList(),
        )

    private fun ImportSourcesUiState.failed(throwable: Throwable): ImportSourcesUiState =
        copy(
            isLoading = false,
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )
}
