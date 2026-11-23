package com.justin13888.capsule.feature.albums.ui.detail

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.feature.albums.data.AlbumPolicyStore
import com.justin13888.capsule.feature.albums.data.AlbumUpgradeCoordinator
import com.justin13888.capsule.feature.albums.data.FederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.model.AlbumMembership
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.model.MembershipEdit
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import java.time.Clock

/**
 * One album, whichever of the three kinds of album it turns out to be.
 *
 * A single view model rather than three, because the *screen* is one screen: the user opened
 * a row that said "album" and everything below the app bar is the same grid. What differs is
 * which affordances exist, and that difference is carried in the state as
 * [AlbumDetailKind] rather than in three near-identical classes that would drift apart.
 *
 * Membership edits go through [AlbumMembership] first and only reach the repository if they
 * would be legal. That ordering is deliberate: a capability *is* which keys MLS delivered, so
 * a client that wrote the change optimistically would be showing the user a role the group
 * never granted.
 */
@Suppress("LongParameterList", "TooManyFunctions")
class AlbumDetailViewModel(
    private val ref: CollectionRef,
    private val albums: AlbumRepository,
    private val policies: AlbumPolicyStore,
    private val upgrades: AlbumUpgradeCoordinator,
    private val federated: FederatedAlbumSource,
    private val sample: LibrarySample,
    private val clock: Clock,
) : ViewModel() {
    private val state = MutableStateFlow<AlbumDetailUiState>(AlbumDetailUiState.Loading)

    /** What the screen is showing right now. */
    val uiState: StateFlow<AlbumDetailUiState> = state.asStateFlow()

    init {
        load()
    }

    /** Re-reads everything this screen shows. */
    fun load() {
        when (ref) {
            is CollectionRef.Container -> observeContainer(ref)
            is CollectionRef.SystemView -> loadView(ref)
            is CollectionRef.Federated -> loadAggregate(ref)
            CollectionRef.Stacks -> state.value = AlbumDetailUiState.Failed(code = null)
        }
    }

    /** Writes a new name onto a container album. */
    fun rename(name: String) = mutateAlbum { album -> album.copy(name = name) }

    /**
     * Deletes the album, unless it is the one import falls back to.
     *
     * The refusal is surfaced rather than hidden: the user has to repoint the default first,
     * and a disabled button with no explanation is how that becomes a support question.
     */
    fun delete(onDeleted: () -> Unit) {
        val album = content()?.album ?: return
        if (!album.isDeletable) {
            message(AlbumDetailMessage.DefaultUndeletable)
            return
        }
        viewModelScope.launch {
            runCatchingAlbum { albums.deleteAlbum(album.id) }?.let { return@launch }
            onDeleted()
        }
    }

    /** Repoints the owner's default-album pointer at this container. */
    fun makeDefault() {
        val album = content()?.album ?: return
        viewModelScope.launch { runCatchingAlbum { albums.setDefaultAlbum(album.id) } }
    }

    /** Invites an account at [capability], refusing locally what MLS would refuse anyway. */
    fun invite(
        recipient: String,
        capability: AlbumCapability,
    ) = applyMembership { album ->
        AlbumMembership.invite(album, recipient, capability, clock.instant())
    }

    /** Removes a member — an MLS `Remove` plus an epoch bump, once it reaches the group. */
    fun removeMember(userId: UserId) = applyMembership { album -> AlbumMembership.remove(album, userId) }

    /** Moves a member to a different capability tier. */
    fun setCapability(
        userId: UserId,
        capability: AlbumCapability,
    ) = applyMembership { album -> AlbumMembership.setCapability(album, userId, capability) }

    /** Issues the upgrade intent, entering quiescence. */
    fun startUpgrade() {
        val album = content()?.album ?: return
        viewModelScope.launch { upgrades.propose(album.id) }
    }

    /** Takes the ceremony's next step: drain, tombstone, fork, replay. */
    fun advanceUpgrade() {
        val album = content()?.album ?: return
        viewModelScope.launch {
            upgrades.advance(album.id)
            val phase = upgrades.phase(album.id).first()
            if (phase is UpgradePhase.Complete) policies.recordUpgrade(album.id, phase.toVersion)
        }
    }

    /** Abandons the ceremony; the album returns to its old version and queued writes flush. */
    fun abortUpgrade() {
        val album = content()?.album ?: return
        viewModelScope.launch { upgrades.abort(album.id) }
    }

    /** Clears the one-off message after the screen has shown it. */
    fun dismissMessage() {
        val current = content() ?: return
        state.value = current.copy(message = null)
    }

    private fun observeContainer(container: CollectionRef.Container) {
        viewModelScope.launch {
            combine(
                albums.album(container.albumId),
                policies.policy(container.albumId),
                upgrades.phase(container.albumId),
            ) { album, policy, phase -> Triple(album, policy, phase) }
                .collect { (album, policy, phase) ->
                    if (album == null) {
                        state.value = AlbumDetailUiState.Failed(code = null)
                        return@collect
                    }
                    val assets = runCatchingAssets { loadAlbumAssets(container) } ?: return@collect
                    state.value =
                        AlbumDetailUiState.Content(
                            title = album.name,
                            kind = AlbumDetailKind.Container,
                            assets = assets.toImmutableList(),
                            isTruncated = assets.size >= MAX_ASSETS,
                            album = album,
                            policy = policy,
                            upgrade = phase,
                            isAdmin = AlbumMembership.viewerIsAdmin(album),
                            isDefault = album.isDefault,
                            message = content()?.message,
                        )
                }
        }
    }

    private fun loadView(view: CollectionRef.SystemView) {
        viewModelScope.launch {
            val sampled = runCatchingAssets { sample.assets().assets.filter(view.kind::matches) } ?: return@launch
            state.value =
                AlbumDetailUiState.Content(
                    title = "",
                    kind = AlbumDetailKind.View(view.kind),
                    assets = sampled.toImmutableList(),
                    isTruncated = false,
                )
        }
    }

    private fun loadAggregate(aggregate: CollectionRef.Federated) {
        viewModelScope.launch {
            val group =
                federated.groups().first().firstOrNull { it.groupId == aggregate.groupId }
                    ?: run {
                        state.value = AlbumDetailUiState.Failed(code = null)
                        return@launch
                    }
            val own = group.ownConstituent
            val assets =
                if (own == null) {
                    emptyList()
                } else {
                    runCatchingAssets { loadAlbumAssets(CollectionRef.Container(own.albumId)) } ?: return@launch
                }
            state.value =
                AlbumDetailUiState.Content(
                    title = group.name,
                    kind = AlbumDetailKind.Aggregate,
                    assets = assets.toImmutableList(),
                    federated = group,
                )
        }
    }

    private suspend fun loadAlbumAssets(container: CollectionRef.Container): List<Asset> {
        val out = ArrayList<Asset>()
        while (out.size < MAX_ASSETS) {
            val window = albums.albumAssets(container.albumId, out.size, minOf(PAGE_SIZE, MAX_ASSETS - out.size))
            if (window.isEmpty()) break
            out += window
        }
        return out
    }

    private fun mutateAlbum(change: (Album) -> Album) {
        val album = content()?.album ?: return
        viewModelScope.launch { runCatchingAlbum { albums.updateAlbum(change(album)) } }
    }

    private fun applyMembership(edit: (Album) -> MembershipEdit) {
        val album = content()?.album ?: return
        when (val outcome = edit(album)) {
            is MembershipEdit.Refused -> message(AlbumDetailMessage.Refused(outcome.reason))
            is MembershipEdit.Applied ->
                viewModelScope.launch { runCatchingAlbum { albums.updateAlbum(outcome.album) } }
        }
    }

    private fun content(): AlbumDetailUiState.Content? = state.value as? AlbumDetailUiState.Content

    private fun message(message: AlbumDetailMessage) {
        val current = content() ?: return
        state.value = current.copy(message = message)
    }

    /** Runs a write, turning a coded failure into a message rather than a crash. */
    private suspend fun runCatchingAlbum(write: suspend () -> Unit): CapsuleError? =
        try {
            write()
            null
        } catch (failure: CapsuleError) {
            message(AlbumDetailMessage.Failed(failure.code?.value))
            failure
        }

    /** Runs a read, turning a coded failure into the screen's own error state. */
    private suspend fun runCatchingAssets(read: suspend () -> List<Asset>): List<Asset>? =
        try {
            read()
        } catch (failure: CapsuleError) {
            state.value = AlbumDetailUiState.Failed(failure.code?.value)
            null
        }

    companion object {
        /**
         * How many of an album's assets are loaded at once.
         *
         * An album can hold a million; the gallery only ever composes a screenful, but the
         * list behind it is still a list. Capping and saying so beats either freezing on
         * open or pretending the album is smaller than it is.
         */
        const val MAX_ASSETS = 1_024

        /** How much of an album is read per call. */
        const val PAGE_SIZE = 256
    }
}
