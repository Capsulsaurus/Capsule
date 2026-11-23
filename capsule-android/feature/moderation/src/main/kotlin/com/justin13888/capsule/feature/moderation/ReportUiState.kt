package com.justin13888.capsule.feature.moderation

import androidx.compose.runtime.Immutable

/** What a report is about. */
enum class ReportSubjectKind {
    /** One asset, named by the hash of its ciphertext. */
    Content,

    /** An account, still evidenced by one of its assets. */
    Account,
}

/** Where a report has got to. */
enum class ReportPhase {
    /** The user is filling it in. */
    Editing,

    /** It is on its way to the home server. */
    Sending,

    /** It reached the admin queue. */
    Sent,

    /** It did not, and the reason is in [ReportUiState.failure]. */
    Failed,
}

/**
 * Everything the report screen draws, as one value.
 *
 * The content hash is a first-class field rather than an implementation detail because it is
 * the *whole* payload: a report names a hash and an album pointer, never plaintext and never
 * decryption material, and a screen that hid that would be a screen a user could not audit.
 * Reporting an account works the same way — an account-level report still has to point an
 * administrator at something they can act on, and the closed set of things that can be is
 * exactly one hash.
 */
@Immutable
data class ReportUiState(
    /** Whatever the destination named — a content hash or an account. */
    val subjectId: String = "",
    /** Which of the two the user is reporting. */
    val kind: ReportSubjectKind = ReportSubjectKind.Content,
    /** The ciphertext hash that leaves this device. */
    val contentHash: String = "",
    /** The account the report is about, where one is known. */
    val accountId: String = "",
    /** Where the asset sits, for an administrator who already has that album. */
    val albumPointer: String = "",
    /** The user's own words, forwarded verbatim. */
    val reason: String = "",
    /** Whether to place a personal block as well as reporting. */
    val alsoBlock: Boolean = false,
    /** Where the submission has got to. */
    val phase: ReportPhase = ReportPhase.Editing,
    /** The failure to explain, or `null`. */
    val failure: ModerationFailure? = null,
    /** Whether the last attempt was refused for naming nothing actionable. */
    val hashMissing: Boolean = false,
) {
    /** Nothing was handed to this screen, so there is nothing to report. */
    val hasNoSubject: Boolean get() = subjectId.isBlank() && contentHash.isBlank()

    /** Whether an account can be blocked alongside the report. */
    val canBlock: Boolean get() = accountId.isNotBlank()

    /** Whether the submit action is live. */
    val canSubmit: Boolean get() = phase != ReportPhase.Sending && contentHash.isNotBlank()

    companion object {
        /**
         * The opening state for a destination that named [subjectId].
         *
         * The kind is *inferred* rather than asked for, because the caller already knows which
         * it handed over and making the user restate it is a question with a known answer. A
         * lowercase-hex identifier of at least [HASH_MIN_LENGTH] characters is a ciphertext
         * hash; anything else is an account. The user can still switch, so a wrong guess costs
         * one tap rather than a dead end.
         */
        fun forSubject(subjectId: String): ReportUiState {
            val looksLikeHash = subjectId.length >= HASH_MIN_LENGTH && subjectId.all { it in HEX_DIGITS }
            return ReportUiState(
                subjectId = subjectId,
                kind = if (looksLikeHash) ReportSubjectKind.Content else ReportSubjectKind.Account,
                contentHash = if (looksLikeHash) subjectId else "",
                accountId = if (looksLikeHash) "" else subjectId,
            )
        }

        private const val HASH_MIN_LENGTH = 32
        private const val HEX_DIGITS = "0123456789abcdef"
    }
}
