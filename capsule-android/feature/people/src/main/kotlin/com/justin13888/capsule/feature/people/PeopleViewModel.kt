package com.justin13888.capsule.feature.people

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.data.PeopleRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The People hub's state machine.
 *
 * The asymmetry the whole screen turns on lives here. A face grouping is a **computed view**
 * — a pure function of the asset set and the model slot behind its embeddings — so nothing
 * on this screen creates or edits one. A person is the **user's** contribution: a name, and
 * which groupings they said were that person. Only the second survives a model swap, which
 * is why a re-clustering can change every row below without touching a word the user typed.
 *
 * The smart albums are computed the same way and stored nowhere, so opening one is a read of
 * a window already in hand rather than a query.
 */
class PeopleViewModel(
    private val people: PeopleRepository,
    private val assets: AssetRepository,
    private val timeline: TimelineRepository,
    private val metadata: MetadataRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(PeopleUiState())

    /** Everything the hub renders. */
    val state: StateFlow<PeopleUiState> = mutableState.asStateFlow()

    /**
     * The built albums, held outside the published state.
     *
     * Opening one must not re-derive anything: membership is a pure function of the window
     * already loaded, so the photographs are in hand and only the selection changes.
     */
    private var memberships: List<SmartAlbumMembership> = emptyList()

    private var loadJob: Job? = null

    init {
        refresh()
    }

    /** The remediation on the error state. */
    fun onRetry() {
        refresh()
    }

    /** A smart album was expanded. */
    fun onOpenSmartAlbum(kind: SmartAlbumKind) {
        val photos =
            memberships
                .firstOrNull { it.kind == kind }
                ?.assets
                .orEmpty()
                .map(PeopleProjection::photoOf)
        mutableState.update {
            it.copy(openedSmartAlbum = kind, openedSmartAlbumPhotos = photos.toImmutableList())
        }
    }

    /** The expanded smart album was collapsed. */
    fun onCloseSmartAlbum() {
        mutableState.update { it.copy(openedSmartAlbum = null, openedSmartAlbumPhotos = emptyPhotos()) }
    }

    /** The naming sheet was asked for, either for a grouping or for an existing person. */
    fun onNameRequested(target: NameTarget) {
        mutableState.update { it.copy(nameTarget = target) }
    }

    /** A person is being merged away; the sheet lists who they could be. */
    fun onMergeOpen(person: PersonCard) {
        mutableState.update { it.copy(mergeSource = person) }
    }

    /** Both sheets closed. */
    fun onDismissSheets() {
        mutableState.update { it.copy(nameTarget = null, mergeSource = null) }
    }

    /** The in-place failure message was acknowledged. */
    fun onDismissActionFailure() {
        mutableState.update { it.copy(actionFailure = null) }
    }

    /**
     * A name was typed.
     *
     * Renaming an existing person is one write. Naming a grouping is two — attach, then name
     * — because `PeopleRepository` has no create call: the person's id has to exist before
     * the attachment can name it, so the client mints it.
     */
    fun onNameSubmit(name: String) {
        val target = mutableState.value.nameTarget ?: return
        val trimmed = name.trim()
        if (trimmed.isEmpty()) return
        onDismissSheets()
        runAction {
            val personId = target.personId?.let(::PersonId) ?: mintPersonId()
            target.clusterId?.let { people.assignCluster(FaceClusterId(it), personId) }
            people.renamePerson(personId, trimmed)
        }
    }

    /** The grouping in the naming sheet is somebody the user already named. */
    fun onAttachCluster(personId: String) {
        val clusterId = mutableState.value.nameTarget?.clusterId ?: return
        onDismissSheets()
        runAction { people.assignCluster(FaceClusterId(clusterId), PersonId(personId)) }
    }

    /** The person in the merge sheet is the same person as [into]. */
    fun onMergeInto(into: String) {
        val from = mutableState.value.mergeSource?.personId ?: return
        onDismissSheets()
        runAction { people.mergePeople(PersonId(from), PersonId(into)) }
    }

    /**
     * A person was removed from the hub.
     *
     * Hiding touches no grouping: the model's work is not thrown away by a presentation
     * decision, and the undo below is a second call rather than a restore.
     */
    fun onHide(person: PersonCard) {
        runAction {
            people.hidePerson(PersonId(person.personId), hidden = true)
            mutableState.update { it.copy(hidden = HiddenPerson(person.personId, person.displayName)) }
        }
    }

    /** The hide was taken back. */
    fun onUndoHide() {
        val person = mutableState.value.hidden ?: return
        mutableState.update { it.copy(hidden = null) }
        runAction { people.hidePerson(PersonId(person.personId), hidden = false) }
    }

    private fun refresh() {
        loadJob?.cancel()
        loadJob =
            viewModelScope.launch {
                mutableState.update { it.copy(phase = PeoplePhase.Loading, failure = null) }
                try {
                    publish(load())
                } catch (error: CapsuleError) {
                    mutableState.update {
                        it.copy(phase = PeoplePhase.Failed, failure = PeopleProjection.failureOf(error))
                    }
                }
            }
    }

    /** One window of the library, plus everything derived from it. */
    private data class Loaded(
        val people: List<PersonCard>,
        val clusters: List<ClusterCard>,
        val albums: List<SmartAlbumCard>,
        val memberships: List<SmartAlbumMembership>,
        val stale: StaleClusterNotice?,
    )

    private suspend fun load(): Loaded {
        val named = people.people().first()
        val clusters = people.clusters().first()
        val window = timeline.timelineWindow(0, WINDOW)
        val tags = window.associate { it.id to metadata.tags(it.id).first() }
        val byId = window.associateBy(Asset::id)
        val built = SmartAlbums.build(window, tags)
        return Loaded(
            people = named.map { person -> personCard(person, byId) },
            clusters = clusters.filter { it.personId == null }.map { cluster -> clusterCard(cluster, byId) },
            albums =
                built.map { membership ->
                    SmartAlbumCard(
                        kind = membership.kind,
                        photoCount = membership.assets.size,
                        cover = membership.assets.firstOrNull()?.let(PeopleProjection::photoOf),
                    )
                },
            memberships = built,
            stale = PeopleProjection.staleNotice(clusters),
        )
    }

    private fun publish(loaded: Loaded) {
        memberships = loaded.memberships
        val empty = loaded.people.isEmpty() && loaded.clusters.isEmpty()
        mutableState.update { current ->
            current.copy(
                phase = if (empty) PeoplePhase.Empty else PeoplePhase.Content,
                people = loaded.people.toImmutableList(),
                clusters = loaded.clusters.toImmutableList(),
                smartAlbums = loaded.albums.toImmutableList(),
                openedSmartAlbumPhotos =
                    current.openedSmartAlbum
                        ?.let { kind -> loaded.memberships.firstOrNull { it.kind == kind } }
                        ?.assets
                        .orEmpty()
                        .map(PeopleProjection::photoOf)
                        .toImmutableList(),
                stale = loaded.stale,
                failure = null,
            )
        }
    }

    private suspend fun personCard(
        person: Person,
        window: Map<AssetId, Asset>,
    ) = PersonCard(
        personId = person.id.value,
        displayName = person.displayName,
        photoCount = person.assetCount,
        cover = cover(person.coverAssetId, window),
    )

    private suspend fun clusterCard(
        cluster: FaceCluster,
        window: Map<AssetId, Asset>,
    ) = ClusterCard(
        clusterId = cluster.id.value,
        photoCount = cluster.assetIds.size,
        cover = cover(cluster.coverAssetId ?: cluster.assetIds.firstOrNull(), window),
        isStale = cluster.status.knownOrNull != ModelOutputStatus.Current,
    )

    /**
     * The tile for a cover asset.
     *
     * Looked up in the window already loaded before falling back to a read, so the common
     * case — a cover that is one of the recent captures — costs nothing extra.
     */
    private suspend fun cover(
        assetId: AssetId?,
        window: Map<AssetId, Asset>,
    ): PersonPhoto? {
        val id = assetId ?: return null
        val asset = window[id] ?: assets.findByUuid(id) ?: return null
        return PeopleProjection.photoOf(asset)
    }

    /**
     * Runs a write, then re-reads.
     *
     * A failure here becomes an in-place message rather than the error *screen*: a rename
     * that was refused is no reason to take the user's people away from them, and a hub that
     * blanked on every failed action would hide the very rows the message is about.
     */
    private fun runAction(block: suspend () -> Unit) {
        viewModelScope.launch {
            try {
                block()
                publish(load())
            } catch (error: CapsuleError) {
                mutableState.update { it.copy(actionFailure = PeopleProjection.failureOf(error)) }
            }
        }
    }

    private companion object {
        /**
         * How much of the library the smart albums are computed over.
         *
         * Bounded because the tag read per asset and the trip clustering are both linear in
         * it, and because an album card showing "four hundred" is no more useful than one
         * showing "forty" — opening it is what the user does next either way.
         */
        const val WINDOW = 400
    }
}

/** The empty photo list, for collapsing an expanded album back down. */
private fun emptyPhotos() = emptyList<PersonPhoto>().toImmutableList()
