package com.justin13888.capsule.feature.moderation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventId
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The appeal's state machine, and the moderation record it is filed against.
 *
 * The submission is two steps and stays two steps: ask the home server for a challenge, sign
 * it with the **master key**, then send. Nothing here can shortcut to a session-authenticated
 * submit — that is the point of the split. An appeal is frequently filed because the session
 * is the thing in dispute, so a session token would be authenticating the wrong claim.
 *
 * A rejected proof is modelled separately from a transport failure because the two need
 * different sentences: one means "nothing was sent, try again", the other means "your key did
 * not answer", and a single generic error would leave the user unsure whether they have now
 * appealed twice.
 */
class AppealViewModel(
    private val moderation: ModerationRepository,
    private val gateway: AppealGateway,
    private val prover: MasterKeyProver,
    caseId: String,
) : ViewModel() {
    private val mutableState = MutableStateFlow(AppealUiState(caseId = caseId))

    /** Everything the screen draws. */
    val state: StateFlow<AppealUiState> = mutableState.asStateFlow()

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
                combine(moderation.events(), moderation.suspensionEffects(), ::Pair)
                    .catch { throwable ->
                        mutableState.update {
                            it.copy(isLoading = false, failure = ModerationFailure.of(throwable))
                        }
                    }.collect { (events, effects) ->
                        mutableState.update {
                            it.copy(
                                isLoading = false,
                                failure = null,
                                events = events.sortedByDescending(ModerationEvent::createdAt),
                                effects = effects,
                            )
                        }
                    }
            }
    }

    /** Edits the statement the administrators will read. */
    fun onStatementChange(value: String) {
        mutableState.update { it.copy(statement = value, statementMissing = false) }
    }

    /**
     * Signs a fresh challenge with the master key and files the appeal.
     *
     * Refused locally when the statement is empty: an appeal with nothing in it gives the
     * administrators nothing to reconsider, and sending one would spend the user's only clear
     * chance to be heard.
     */
    fun submit() {
        val current = mutableState.value
        val case = current.case ?: return
        if (!current.canSubmit) return
        if (current.statement.isBlank()) {
            mutableState.update { it.copy(statementMissing = true) }
            return
        }
        mutableState.update {
            it.copy(phase = AppealPhase.Proving, failure = null, proofRejected = false, statementMissing = false)
        }
        viewModelScope.launch { file(case.id, current.statement) }
    }

    /** Puts the form back in front of the user after a refusal, with what they wrote intact. */
    fun retry() {
        mutableState.update { it.copy(phase = AppealPhase.Editing, failure = null, proofRejected = false) }
    }

    private suspend fun file(
        caseId: ModerationEventId,
        statement: String,
    ) {
        val proof =
            runCatching {
                val challenge = gateway.challenge(caseId)
                prover.sign(challenge)
            }.getOrElse { throwable ->
                mutableState.update { it.failedProof(throwable) }
                return
            }
        mutableState.update { it.copy(phase = AppealPhase.Sending) }
        runCatching { gateway.submit(AppealRequest(caseId, statement, proof)) }
            .onSuccess { receipt ->
                mutableState.update { it.copy(phase = AppealPhase.Sent, reference = receipt.reference) }
            }.onFailure { throwable ->
                mutableState.update { it.failedProof(throwable) }
            }
    }

    /**
     * The refusal, split by whether the master key was the thing that failed.
     *
     * [AppealProofRejected] is the one case where the user needs to be told that *nothing was
     * sent*: every other failure leaves the question of whether the appeal landed open, and
     * telling someone to retry a submission that may already have arrived is how a queue fills
     * with duplicates.
     */
    private fun AppealUiState.failedProof(throwable: Throwable): AppealUiState =
        copy(
            phase = AppealPhase.Failed,
            proofRejected = throwable is AppealProofRejected,
            failure = if (throwable is AppealProofRejected) null else ModerationFailure.of(throwable),
        )
}
