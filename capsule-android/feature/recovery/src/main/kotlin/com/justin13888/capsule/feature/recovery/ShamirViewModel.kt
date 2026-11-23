package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.security.SecureRandom
import java.util.Locale

/**
 * Everything the guardian-share screen draws, as one value.
 *
 * [threshold] and [total] are the *proposed* scheme while the user is choosing, and the
 * enrolment's own numbers once shares exist. Keeping the proposal separate is what lets the
 * screen refuse an impossible combination structurally — the generate action is simply not
 * live — rather than coercing it into something the user did not ask for.
 */
@Immutable
data class ShamirUiState(
    /** How many shares should be needed to recover. */
    val threshold: Int = DEFAULT_THRESHOLD,
    /** How many shares to create. */
    val total: Int = DEFAULT_TOTAL,
    /** The current enrolment, or `null` when the secret has never been split. */
    val enrollment: ShamirEnrollment? = null,
    /** Which share's holder field is open, if any. */
    val editingShare: Int? = null,
    /** What the user has typed into that holder field. */
    val holderDraft: String = "",
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
) {
    /** Whether the proposed scheme could exist. */
    val isSchemeValid: Boolean
        get() = threshold >= ShamirSplitter.MIN_THRESHOLD && total in threshold..ShamirSplitter.MAX_SHARES

    /** Nothing has been split yet. */
    val isEmpty: Boolean get() = enrollment == null

    /** Whether enough shares are off this device to rebuild the secret without it. */
    val isDistributed: Boolean get() = enrollment?.isDistributed == true

    companion object {
        /** The default scheme the design names: any two of three. */
        const val DEFAULT_THRESHOLD = 2

        /** The default share count. */
        const val DEFAULT_TOTAL = 3

        /** The largest share count this screen offers, which is a usability bound, not a maths one. */
        const val OFFERED_MAXIMUM = 8
    }
}

/**
 * The guardian-share screen's state machine.
 *
 * Two things it deliberately does not do. It never asks the server for anything — the split
 * and any future reconstruction are entirely client-side, and a round trip here would be a
 * round trip that could carry more than one share. And it never persists a share: they exist
 * in memory until the user hands them out, because a share stored beside the library it
 * protects protects nothing.
 *
 * The proposed scheme is clamped as the user adjusts it rather than validated on submit, so
 * "shares needed" can never exceed "shares created" even momentarily — an intermediate state
 * that would let a stray tap generate a scheme nobody can satisfy.
 */
class ShamirViewModel(
    private val store: ShamirShareStore,
    private val gateway: RecoveryRewrapGateway,
    private val random: SecureRandom = SecureRandom(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(ShamirUiState())

    /** Everything the screen draws. */
    val state: StateFlow<ShamirUiState> = mutableState.asStateFlow()

    init {
        viewModelScope.launch {
            store.enrollment.collect { current ->
                mutableState.update {
                    it.copy(
                        enrollment = current,
                        threshold = current?.threshold ?: it.threshold,
                        total = current?.total ?: it.total,
                    )
                }
            }
        }
    }

    /** Raises or lowers how many shares are needed, never past how many exist. */
    fun onThresholdChange(value: Int) {
        mutableState.update {
            it.copy(threshold = value.coerceIn(ShamirSplitter.MIN_THRESHOLD, it.total))
        }
    }

    /** Raises or lowers how many shares to create, never below how many are needed. */
    fun onTotalChange(value: Int) {
        mutableState.update {
            it.copy(total = value.coerceIn(it.threshold, ShamirUiState.OFFERED_MAXIMUM))
        }
    }

    /**
     * Splits a freshly minted secret into the proposed scheme.
     *
     * Minting rather than splitting the live secret, because the live one is not on this
     * device in a form that could be split — it is held by the user, and the escrow holds only
     * a wrap of the master key under it. The gateway is the seam where the real flow reads the
     * seed instead.
     */
    fun generate() {
        val current = mutableState.value
        if (!current.isSchemeValid) return
        runCatching {
            val secret = gateway.mint()
            val shares =
                ShamirSplitter.split(
                    secret = secret.phrase.toByteArray(),
                    threshold = current.threshold,
                    total = current.total,
                    random = random,
                )
            ShamirEnrollment(
                threshold = current.threshold,
                total = current.total,
                shares = shares.map { ShamirShare(index = it.x, payload = it.payload.toHex()) },
            )
        }.onSuccess { enrollment ->
            store.replace(enrollment)
            mutableState.update { it.copy(failure = null) }
        }.onFailure { throwable ->
            mutableState.update { it.copy(failure = RecoveryFailure.of(throwable)) }
        }
    }

    /** Opens the holder field for one share. */
    fun onEditHolder(index: Int?) {
        mutableState.update { it.copy(editingShare = index, holderDraft = "") }
    }

    /** Edits who is taking the share currently being handed over. */
    fun onHolderDraftChange(value: String) {
        mutableState.update { it.copy(holderDraft = value) }
    }

    /** Records that a share has left this device. */
    fun markDelivered(index: Int) {
        val holder = mutableState.value.holderDraft.trim()
        if (holder.isEmpty()) return
        store.markDelivered(index, holder)
        mutableState.update { it.copy(editingShare = null, holderDraft = "") }
    }

    /** Lowercase hex, in a fixed locale so a share reads the same on every device. */
    private fun ByteArray.toHex(): String = joinToString("") { byte -> String.format(Locale.ROOT, "%02x", byte) }
}
