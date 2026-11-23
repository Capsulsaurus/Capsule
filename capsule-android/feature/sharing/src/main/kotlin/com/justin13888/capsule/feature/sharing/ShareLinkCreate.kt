package com.justin13888.capsule.feature.sharing

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareScope
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Duration
import java.time.Instant

/** One album offered as a share scope. */
@Immutable
data class AlbumOption(
    /** The album's id, re-wrapped from the navigation key at the feature boundary. */
    val id: AlbumId,
    /** The album's name, or empty for the nameless default album. */
    val name: String,
)

/** Which of the two things a link may grant read access to. */
enum class ShareScopeChoice {
    /** One asset, and nothing around it. */
    SingleAsset,

    /** A whole container album, including whatever is added to it later. */
    WholeAlbum,
}

/**
 * How long a new link stays live.
 *
 * Presets rather than a free date, because the decision being made is "how long do I want to
 * be exposed", and a calendar answers a different question — one whose most common answer, a
 * date the user picks without thinking, is the link that outlives its reason to exist.
 */
enum class ShareExpiry(
    /** How long past creation the link stops serving; `null` never expires. */
    val duration: Duration?,
) {
    /** No expiry. Revocation is then the only way to end it. */
    Never(null),

    /** A day — long enough to hand something over, short enough to forget safely. */
    OneDay(Duration.ofDays(1)),

    /** A week. */
    OneWeek(Duration.ofDays(7)),

    /** A month. */
    OneMonth(Duration.ofDays(30)),
    ;

    /** The instant this preset expires at, measured from [now]. */
    fun expiresAt(now: Instant): Instant? = duration?.let(now::plus)
}

/**
 * Everything the create-share-link screen draws, as one value.
 *
 * [strippedFields] is state the screen *reads* and can never write. The privacy strip on a
 * share is mandatory and has no opt-out, so exposing it as anything the form could change
 * would describe a system that does not exist — and a disclosure the user can untick is worse
 * than no disclosure, because it teaches them the fields are sometimes kept.
 */
@Immutable
data class ShareLinkCreateUiState(
    /** Whether the album list is still being read. */
    val isLoading: Boolean = true,
    /** Why the form cannot be shown or submitted, or `null`. */
    val failure: SharingFailure? = null,
    /** The asset this screen was opened for, when it was opened from one. */
    val assetId: AssetId? = null,
    /** The container albums a link may be scoped to. */
    val albums: List<AlbumOption> = emptyList(),
    /** Which kind of scope the user picked. */
    val scope: ShareScopeChoice = ShareScopeChoice.WholeAlbum,
    /** The album picked for [ShareScopeChoice.WholeAlbum]. */
    val selectedAlbumId: AlbumId? = null,
    /** The optional passphrase. Never leaves the device; it wraps the secret locally. */
    val passphrase: String = "",
    /** The chosen expiry preset. */
    val expiry: ShareExpiry = ShareExpiry.Never,
    /** Whether a creation is in flight. */
    val isSubmitting: Boolean = false,
    /** The link that was just minted, carrying its one and only fragment secret. */
    val created: ShareLink? = null,
) {
    /** What the disclosure lists. Always the whole set; never a selection. */
    val strippedFields: List<StrippedField> = StrippedField.ALL

    /** Whether a single-asset scope can be offered at all. */
    val canScopeToAsset: Boolean get() = assetId != null

    /** Whether the form describes a scope the repository can act on. */
    val canSubmit: Boolean
        get() =
            !isSubmitting &&
                !isLoading &&
                when (scope) {
                    ShareScopeChoice.SingleAsset -> assetId != null
                    ShareScopeChoice.WholeAlbum -> selectedAlbumId != null
                }

    /** The scope the form currently describes, or `null` when it does not describe one yet. */
    internal fun resolvedScope(): ShareScope? =
        when (scope) {
            ShareScopeChoice.SingleAsset -> assetId?.let(ShareScope::SingleAsset)
            ShareScopeChoice.WholeAlbum -> selectedAlbumId?.let(ShareScope::WholeAlbum)
        }
}

/**
 * The create-share-link form's state machine.
 *
 * [clock] is a constructor default rather than an injected binding so a test can pin the
 * expiry arithmetic without the app graph having to own a clock that nothing else needs. The
 * default is the real one: an expiry is a promise about wall-clock time, and a link that
 * expired against a fixture's idea of "now" would be a link that never worked.
 */
class ShareLinkCreateViewModel(
    private val sharing: SharingRepository,
    private val albums: AlbumRepository,
    /** The asset to scope to, when the screen was reached from one. */
    assetId: AssetId? = null,
    /** The album to pre-select, when the screen was reached from one. */
    albumId: AlbumId? = null,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState =
        MutableStateFlow(
            ShareLinkCreateUiState(
                assetId = assetId,
                selectedAlbumId = albumId,
                // Arriving from an asset means the user already chose the thing to share.
                scope = if (assetId != null) ShareScopeChoice.SingleAsset else ShareScopeChoice.WholeAlbum,
            ),
        )

    /** Everything the form draws. */
    val state: StateFlow<ShareLinkCreateUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Re-reads the album list. Also the remediation behind the error state's action. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                albums
                    .albums()
                    .catch { throwable ->
                        val failure = SharingFailure.of(throwable)
                        mutableState.update { it.copy(isLoading = false, failure = failure) }
                    }.collect { list -> mutableState.update { it.withAlbums(list) } }
            }
    }

    /** Switches between sharing one item and sharing a whole album. */
    fun selectScope(choice: ShareScopeChoice) {
        // Refusing the asset scope when there is no asset keeps the form from describing a
        // link the repository would have to reject.
        if (choice == ShareScopeChoice.SingleAsset && mutableState.value.assetId == null) return
        mutableState.update { it.copy(scope = choice) }
    }

    /** Chooses which album an album-scoped link points at. */
    fun selectAlbum(albumId: AlbumId) {
        mutableState.update { it.copy(selectedAlbumId = albumId, scope = ShareScopeChoice.WholeAlbum) }
    }

    /** Records the typed passphrase. Blank means the link is not passphrase-wrapped. */
    fun setPassphrase(value: String) {
        mutableState.update { it.copy(passphrase = value) }
    }

    /** Chooses how long the link stays live. */
    fun selectExpiry(expiry: ShareExpiry) {
        mutableState.update { it.copy(expiry = expiry) }
    }

    /**
     * Mints the link.
     *
     * The passphrase is passed straight through and never stored: the repository wraps the
     * read secret with it locally, and the server is never in the password-trust path.
     */
    fun create() {
        val current = mutableState.value
        val scope = current.resolvedScope() ?: return
        if (current.isSubmitting) return
        mutableState.update { it.copy(isSubmitting = true, failure = null) }
        viewModelScope.launch {
            runCatching {
                sharing.createShareLink(
                    scope = scope,
                    expiresAt = current.expiry.expiresAt(clock.instant()),
                    passphrase = current.passphrase.takeIf(String::isNotBlank),
                )
            }.onSuccess { link -> mutableState.update { it.copy(isSubmitting = false, created = link) } }
                .onFailure { throwable ->
                    mutableState.update { it.copy(isSubmitting = false, failure = SharingFailure.of(throwable)) }
                }
        }
    }

    /** Acknowledges the created link so the screen does not navigate to it twice. */
    fun consumeCreated() {
        mutableState.update { it.copy(created = null) }
    }

    private fun ShareLinkCreateUiState.withAlbums(list: List<Album>): ShareLinkCreateUiState {
        // Only containers. A view album holds no key and owns no assets, so it is not a
        // sharing boundary and a link scoped to one would grant access to nothing.
        val options =
            list
                .filter { it.kind.knownOrNull == AlbumKind.Container }
                .map { AlbumOption(it.id, it.name) }
        val selected = selectedAlbumId?.takeIf { id -> options.any { it.id == id } }
        return copy(
            isLoading = false,
            failure = null,
            albums = options,
            selectedAlbumId = selected ?: options.firstOrNull()?.id,
        )
    }
}
