package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.core.model.UploadLink
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Duration
import java.time.Instant

/** How many bytes one megabyte stands for in this form. */
internal const val BYTES_PER_MEGABYTE: Long = 1_000_000L

/**
 * How long a new upload link accepts deposits for.
 *
 * Presets rather than a free date. The question being answered is "how long do I want to be
 * paying for whatever arrives", and a calendar answers a different one — whose commonest
 * answer is a link that outlives the reason it was made.
 */
enum class LinkExpiry(
    /** How long past creation the link closes; `null` never closes. */
    val duration: Duration?,
) {
    /** No expiry. Revocation is then the only way to close it. */
    Never(null),

    /** A day. */
    OneDay(Duration.ofDays(1)),

    /** A week — the default, because an inbox left open is quota left spendable. */
    OneWeek(Duration.ofDays(7)),

    /** A month. */
    OneMonth(Duration.ofDays(30)),
    ;

    /** The instant this preset closes at, measured from [now]. */
    fun expiresAt(now: Instant): Instant? = duration?.let(now::plus)
}

/**
 * One optional numeric cap as the user is typing it.
 *
 * Blank means *unbounded*, which is a deliberate value rather than a missing one: the link's
 * owner pays for the bytes, so the design leaves the ceiling to them. Anything else has to be
 * a positive whole number, and a half-typed one is an error the form shows rather than a
 * value it silently rounds.
 */
@Immutable
data class CapEntry(
    /** Exactly what is in the field. */
    val raw: String = "",
) {
    /** Whether the field is empty, and therefore means "no limit". */
    val isUnbounded: Boolean get() = raw.isBlank()

    /** The value, or `null` when the field is empty or does not parse. */
    val value: Long? get() = raw.trim().toLongOrNull()?.takeIf { it > 0 }

    /** Whether the field holds something that is neither empty nor a usable number. */
    val isInvalid: Boolean get() = !isUnbounded && value == null
}

/**
 * Which of the three numeric caps a form edit is about.
 *
 * One enum rather than three setters because the three fields differ only in which ceiling
 * they name — parsing, validation and the meaning of "empty" are identical — and three
 * near-identical callbacks is three places for them to drift apart.
 */
enum class CapKind {
    /** The ceiling on everything deposited through the link. */
    TotalBytes,

    /** The ceiling on how many files may arrive. */
    FileCount,

    /** The ceiling on any one file. */
    FileSize,
}

/**
 * Everything the create-upload-link form draws, as one value.
 */
@Immutable
data class UploadLinkCreateUiState(
    /** When the link stops accepting deposits. */
    val expiry: LinkExpiry = LinkExpiry.OneWeek,
    /** The ceiling on everything deposited through the link, in megabytes. */
    val maxTotal: CapEntry = CapEntry(),
    /** The ceiling on how many files may arrive. */
    val maxFiles: CapEntry = CapEntry(),
    /** The ceiling on any one file, in megabytes. */
    val maxFileSize: CapEntry = CapEntry(),
    /** Whether the link closes after one deposit session. */
    val singleUse: Boolean = false,
    /** The optional passphrase. An abuse gate, never a confidentiality layer. */
    val passphrase: String = "",
    /** Whether a provisioning is in flight. */
    val isSubmitting: Boolean = false,
    /** Why the last attempt failed, or `null`. */
    val failure: DropsFailure? = null,
    /** The link that was just provisioned. */
    val created: UploadLink? = null,
) {
    /** The entry behind one ceiling. */
    fun entry(kind: CapKind): CapEntry =
        when (kind) {
            CapKind.TotalBytes -> maxTotal
            CapKind.FileCount -> maxFiles
            CapKind.FileSize -> maxFileSize
        }

    /** Whether every cap the user typed is usable. */
    val hasInvalidCap: Boolean get() = maxTotal.isInvalid || maxFiles.isInvalid || maxFileSize.isInvalid

    /** Whether the form can be submitted right now. */
    val canSubmit: Boolean get() = !isSubmitting && !hasInvalidCap

    /** The caps this form describes, measured from [now]. */
    internal fun caps(now: Instant): LinkCaps =
        LinkCaps(
            expiresAt = expiry.expiresAt(now),
            maxTotalBytes = maxTotal.value?.times(BYTES_PER_MEGABYTE),
            maxFileCount = maxFiles.value?.toInt(),
            maxFileSize = maxFileSize.value?.times(BYTES_PER_MEGABYTE),
            singleUse = singleUse,
        )
}

/**
 * The create-upload-link form's state machine.
 *
 * The caps are the whole point of this screen and are worth stating plainly: they are what
 * bounds a leaked link to wasted quota and inbox clutter. Nothing an upload link admits can
 * reach the library, because adoption happens on a trusted client and nowhere else.
 */
class UploadLinkCreateViewModel(
    private val sharing: SharingRepository,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(UploadLinkCreateUiState())

    /** Everything the form draws. */
    val state: StateFlow<UploadLinkCreateUiState> = mutableState.asStateFlow()

    /** Chooses how long the link accepts deposits. */
    fun selectExpiry(expiry: LinkExpiry) {
        mutableState.update { it.copy(expiry = expiry) }
    }

    /** Records one ceiling exactly as typed, valid or not. */
    fun setCap(
        kind: CapKind,
        raw: String,
    ) {
        mutableState.update { current ->
            when (kind) {
                CapKind.TotalBytes -> current.copy(maxTotal = CapEntry(raw))
                CapKind.FileCount -> current.copy(maxFiles = CapEntry(raw))
                CapKind.FileSize -> current.copy(maxFileSize = CapEntry(raw))
            }
        }
    }

    /** Sets whether the link closes after one deposit session. */
    fun setSingleUse(singleUse: Boolean) {
        mutableState.update { it.copy(singleUse = singleUse) }
    }

    /** Records the typed passphrase. Blank means no abuse gate. */
    fun setPassphrase(value: String) {
        mutableState.update { it.copy(passphrase = value) }
    }

    /**
     * Provisions the link.
     *
     * The passphrase goes through as an **abuse gate**: the server holds a verifier and a
     * guest proves possession at session creation. It adds no confidentiality — a guest
     * already encrypts every file before it leaves their browser — and it is a different
     * thing entirely from a share link's passphrase, which is why the repository takes it
     * through a different method.
     */
    fun create() {
        val current = mutableState.value
        if (!current.canSubmit) return
        mutableState.update { it.copy(isSubmitting = true, failure = null) }
        viewModelScope.launch {
            runCatching {
                sharing.createUploadLink(
                    caps = current.caps(clock.instant()),
                    passphrase = current.passphrase.takeIf(String::isNotBlank),
                )
            }.onSuccess { link -> mutableState.update { it.copy(isSubmitting = false, created = link) } }
                .onFailure { throwable ->
                    mutableState.update { it.copy(isSubmitting = false, failure = DropsFailure.of(throwable)) }
                }
        }
    }

    /** Acknowledges the created link so the screen does not navigate to it twice. */
    fun consumeCreated() {
        mutableState.update { it.copy(created = null) }
    }
}
