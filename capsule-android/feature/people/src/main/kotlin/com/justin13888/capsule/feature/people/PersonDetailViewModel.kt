package com.justin13888.capsule.feature.people

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.data.PeopleRepository
import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.PersonId
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * One person's own screen.
 *
 * The suggestion list is the reason this screen carries a metadata repository at all. Model
 * suggestions and the user's own tags are separate fields, structurally, so that a model can
 * never overwrite what the user wrote — and the consequence is that turning a suggestion into
 * a tag has to be an action somebody takes. That action is here, it is one row at a time, and
 * it is never automatic.
 */
class PersonDetailViewModel(
    private val personId: String,
    private val people: PeopleRepository,
    private val metadata: MetadataRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(PersonDetailUiState())

    /** Everything the screen renders. */
    val state: StateFlow<PersonDetailUiState> = mutableState.asStateFlow()

    private var loadJob: Job? = null

    init {
        refresh()
    }

    /** The remediation on the error state. */
    fun onRetry() {
        refresh()
    }

    /** The rename sheet was opened or dismissed. */
    fun onRenameRequested(open: Boolean) {
        mutableState.update { it.copy(renaming = open) }
    }

    /** A new name was typed. The name is the user's; a model never writes it. */
    fun onRenameSubmit(name: String) {
        val trimmed = name.trim()
        if (trimmed.isEmpty()) return
        onRenameRequested(open = false)
        runAction { people.renamePerson(PersonId(personId), trimmed) }
    }

    /**
     * A suggestion was kept.
     *
     * Copies the entry into the user's own set under a fresh, user-scoped add id, and leaves
     * the model's entry alone: the two namespaces stay separate afterwards, so a later re-run
     * of the model cannot touch the tag the user has now claimed.
     */
    fun onPromote(suggestion: AiSuggestion) {
        runAction { metadata.promoteAiTag(AssetId(suggestion.assetId), suggestion.addId()) }
    }

    /** A suggestion was rejected — an observed-remove on the model's own set. */
    fun onDismissSuggestion(suggestion: AiSuggestion) {
        runAction { metadata.dismissAiTag(AssetId(suggestion.assetId), suggestion.addId()) }
    }

    /** The in-place failure message was acknowledged. */
    fun onDismissActionFailure() {
        mutableState.update { it.copy(actionFailure = null) }
    }

    private fun refresh() {
        loadJob?.cancel()
        loadJob =
            viewModelScope.launch {
                mutableState.update { it.copy(phase = PeoplePhase.Loading, failure = null) }
                try {
                    load()
                } catch (error: CapsuleError) {
                    mutableState.update {
                        it.copy(phase = PeoplePhase.Failed, failure = PeopleProjection.failureOf(error))
                    }
                }
            }
    }

    private suspend fun load() {
        val id = PersonId(personId)
        // Hidden people still resolve by id: hiding removes them from the hub, not from the
        // library, so a link to one has to keep working.
        val person = people.person(id).first() ?: throw LocalError.NotFound("person $personId")
        val assets = people.personAssets(id, 0, WINDOW).sortedByDescending(Asset::captureTimestamp)
        val tags = assets.associate { it.id to metadata.tags(it.id).first() }
        mutableState.update {
            it.copy(
                phase = if (assets.isEmpty()) PeoplePhase.Empty else PeoplePhase.Content,
                displayName = person.displayName,
                photos = assets.map(PeopleProjection::photoOf).toImmutableList(),
                suggestions =
                    PeopleProjection
                        .suggestions(assets.map(Asset::id), tags, MAX_SUGGESTIONS)
                        .toImmutableList(),
                failure = null,
            )
        }
    }

    /**
     * Runs a write, then re-reads.
     *
     * A refused write becomes an in-place message rather than the error screen: losing a
     * person's photographs because a tag could not be promoted would be a far worse answer
     * than a line of text saying the promotion did not happen.
     */
    private fun runAction(block: suspend () -> Unit) {
        viewModelScope.launch {
            try {
                block()
                load()
            } catch (error: CapsuleError) {
                mutableState.update { it.copy(actionFailure = PeopleProjection.failureOf(error)) }
            }
        }
    }

    private companion object {
        /** How many of a person's photographs one screen holds. */
        const val WINDOW = 400

        /** How many distinct suggestions are offered before the list stops being readable. */
        const val MAX_SUGGESTIONS = 12
    }
}

/** Rebuilds the add id a suggestion targets, which is what a promote or a dismiss is keyed by. */
private fun AiSuggestion.addId(): AddId = AddId(DeviceId(deviceId), counter)
