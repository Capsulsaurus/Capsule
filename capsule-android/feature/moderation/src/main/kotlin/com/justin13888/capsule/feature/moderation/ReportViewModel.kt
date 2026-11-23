package com.justin13888.capsule.feature.moderation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.model.BlobHash
import com.justin13888.capsule.core.model.UserId
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The report form's state machine.
 *
 * The submit path is deliberately narrow: one repository call carrying a hash, a pointer and
 * the user's words, and nothing else. There is no branch anywhere in this class that could
 * attach a caption, a thumbnail or a key, because the report contract does not have a
 * parameter for one — which is the property that keeps reporting from becoming a decryption
 * side channel.
 *
 * The optional block is applied **before** the report is sent. A user who wants both wants
 * the block whether or not the far server ever acts, and ordering it first means a refused
 * report still leaves them with the protection they asked for.
 */
class ReportViewModel(
    private val moderation: ModerationRepository,
    subjectId: String,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ReportUiState.forSubject(subjectId))

    /** Everything the screen draws. */
    val state: StateFlow<ReportUiState> = mutableState.asStateFlow()

    /** Switches between reporting one asset and reporting the account behind it. */
    fun onKindChange(kind: ReportSubjectKind) {
        mutableState.update { it.copy(kind = kind, hashMissing = false) }
    }

    /** Edits the ciphertext hash the report names. */
    fun onContentHashChange(value: String) {
        mutableState.update { it.copy(contentHash = value.trim(), hashMissing = false) }
    }

    /** Edits the album pointer that locates the asset for an administrator who has access. */
    fun onPointerChange(value: String) {
        mutableState.update { it.copy(albumPointer = value) }
    }

    /** Edits the free-text reason forwarded to the administrators. */
    fun onReasonChange(value: String) {
        mutableState.update { it.copy(reason = value) }
    }

    /** Chooses whether a personal block accompanies the report. */
    fun onAlsoBlockChange(value: Boolean) {
        mutableState.update { it.copy(alsoBlock = value) }
    }

    /**
     * Sends the report, and places the block first when one was asked for.
     *
     * A report with no hash is refused here rather than at the repository, so the user is told
     * what is missing instead of watching a call fail: there is nothing an administrator can
     * do with a report that names nothing they can act on.
     */
    fun submit() {
        val current = mutableState.value
        if (current.phase == ReportPhase.Sending) return
        if (current.contentHash.isBlank()) {
            mutableState.update { it.copy(hashMissing = true) }
            return
        }
        mutableState.update { it.copy(phase = ReportPhase.Sending, failure = null, hashMissing = false) }
        viewModelScope.launch {
            runCatching {
                if (current.alsoBlock && current.canBlock) moderation.blockUser(UserId(current.accountId))
                moderation.report(
                    contentHash = BlobHash(current.contentHash),
                    albumPointer = current.albumPointer.takeIf(String::isNotBlank),
                    reason = current.reason.takeIf(String::isNotBlank),
                )
            }.onSuccess {
                mutableState.update { it.copy(phase = ReportPhase.Sent, failure = null) }
            }.onFailure { throwable ->
                mutableState.update {
                    it.copy(phase = ReportPhase.Failed, failure = ModerationFailure.of(throwable))
                }
            }
        }
    }

    /** Puts the form back in front of the user after a refusal, with what they typed intact. */
    fun retry() {
        mutableState.update { it.copy(phase = ReportPhase.Editing, failure = null) }
    }
}
