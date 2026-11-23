package com.justin13888.capsule.feature.people

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * One photograph, reduced to what a grid or a cover tile draws.
 *
 * A projection rather than the `Asset` itself: Compose cannot prove a domain record with
 * list-typed fields is stable, so passing one into a cell would defeat skipping across the
 * whole viewport.
 */
@Immutable
data class PersonPhoto(
    /** The asset's id, as the shell's navigation keys carry it. */
    val assetId: String,
    /** Width over height — the only thing the layout engine consumes. */
    val aspectRatio: Float,
    /** `0xRRGGBB` of the asset's dominant colour, drawn until a thumbnail exists. */
    val placeholder: Int,
    /** An already-formatted running time for a video, or `null` for a still. */
    val videoDuration: String? = null,
)

/** Someone the user has named. */
@Immutable
data class PersonCard(
    /** The person's id, as a navigation key carries it. */
    val personId: String,
    /** What the user called them. Never the model's guess. */
    val displayName: String,
    /** How many photographs they appear in across every attached cluster. */
    val photoCount: Int,
    /** The tile to draw for them, or `null` when the cover asset is not in reach. */
    val cover: PersonPhoto?,
)

/**
 * A face grouping nobody has named yet.
 *
 * Stale groupings are listed rather than hidden. They are the regeneration queue, and a user
 * who watched a person disappear after an app update is owed the reason — so the badge says
 * the grouping came from a replaced model version instead of the row silently vanishing.
 */
@Immutable
data class ClusterCard(
    /** The grouping's handle within the current computation. */
    val clusterId: String,
    /** How many photographs it holds. */
    val photoCount: Int,
    /** The tile to draw for it. */
    val cover: PersonPhoto?,
    /** Whether the model version behind it has been replaced. */
    val isStale: Boolean,
)

/** One automatically built album, as the hub lists it. */
@Immutable
data class SmartAlbumCard(
    /** Which album. */
    val kind: SmartAlbumKind,
    /** How many photographs it currently matches. Computed, never stored. */
    val photoCount: Int,
    /** The tile to draw for it. */
    val cover: PersonPhoto?,
)

/**
 * The face groupings the library is not querying.
 *
 * Present on the state because a screen that simply showed fewer people would be lying by
 * omission: when a model version changes, its outputs are excluded from queries until they
 * are regenerated, and that is a fact about the library rather than about this screen.
 */
@Immutable
data class StaleClusterNotice(
    /** How many groupings are being held out. */
    val staleCount: Int,
    /** The superseded `model_id model_version` slots behind them. */
    val slots: ImmutableList<String>,
)

/** What the People hub is currently doing. */
enum class PeoplePhase {
    /** The first load has not finished. */
    Loading,

    /** Nothing has been grouped yet. */
    Empty,

    /** There are people, groupings or albums to show. */
    Content,

    /** The load itself failed. */
    Failed,
}

/**
 * Why something failed, in terms the screen can localize.
 *
 * The view model collapses the repository's `error.*` code down to one of these rather than
 * publishing the code, so the screen's `when` is exhaustive and no unrecognised code can
 * reach a user as a raw token — and never an HTTP status, which says nothing anyone can act
 * on.
 */
enum class PeopleFailure {
    /** No usable network path. */
    Offline,

    /** The account is administratively suspended, so nothing may be written. */
    Suspended,

    /** Past the quota grace window: metadata growth is refused. */
    QuotaLocked,

    /** The library holds state this build cannot read. */
    Unsupported,

    /** The row the action named is not there any more. */
    Missing,

    /** Anything the catalogue does not name. */
    Unknown,
}

/** Which naming sheet is up, and what it is naming. */
@Immutable
data class NameTarget(
    /** The person being renamed, or `null` when an unnamed grouping is being named. */
    val personId: String?,
    /** The grouping being named, or `null` when an existing person is being renamed. */
    val clusterId: String?,
    /** What the box starts with. */
    val initialName: String,
)

/** A person the user just hid, held so the action can be taken back. */
@Immutable
data class HiddenPerson(
    /** Their id, for the undo. */
    val personId: String,
    /** Their name, for the message. */
    val displayName: String,
)

/**
 * Everything the People hub renders, as one immutable value.
 *
 * One state object rather than several flows, because these fields are read together on one
 * frame: a smart-album count that recomposed against a different library window than the
 * grid under it would show a number that does not match the photographs.
 */
@Immutable
data class PeopleUiState(
    /** Which screen to draw. */
    val phase: PeoplePhase = PeoplePhase.Loading,
    /** The people the user has named. */
    val people: ImmutableList<PersonCard> = persistentListOf(),
    /** The groupings nobody has named. */
    val clusters: ImmutableList<ClusterCard> = persistentListOf(),
    /** The automatically built albums. */
    val smartAlbums: ImmutableList<SmartAlbumCard> = persistentListOf(),
    /** The album whose photographs are expanded, or `null` while the hub is collapsed. */
    val openedSmartAlbum: SmartAlbumKind? = null,
    /** The opened album's photographs. */
    val openedSmartAlbumPhotos: ImmutableList<PersonPhoto> = persistentListOf(),
    /** The groupings held out of queries, or `null` when every slot is canonical. */
    val stale: StaleClusterNotice? = null,
    /** The naming sheet, or `null`. */
    val nameTarget: NameTarget? = null,
    /** The person being merged away, or `null`. */
    val mergeSource: PersonCard? = null,
    /** The person just hidden, or `null`. */
    val hidden: HiddenPerson? = null,
    /** Why the load failed, or `null`. */
    val failure: PeopleFailure? = null,
    /** Why the last action failed, or `null`. Shown in place, without blanking the hub. */
    val actionFailure: PeopleFailure? = null,
)

/**
 * One model suggestion on one photograph.
 *
 * Carries the add it targets rather than just the word, because a remove or a promotion is
 * keyed by the add: two devices can suggest the same word independently, and acting on "the
 * tag `beach`" would touch an entry this device never observed.
 */
@Immutable
data class AiSuggestion(
    /** The photograph the suggestion is on. */
    val assetId: String,
    /** The suggested word. */
    val tag: String,
    /** The adding device, half of the add id. */
    val deviceId: String,
    /** The per-device counter, the other half of the add id. */
    val counter: Long,
)

/** Everything one person's own screen renders. */
@Immutable
data class PersonDetailUiState(
    /** Which screen to draw. */
    val phase: PeoplePhase = PeoplePhase.Loading,
    /** What the user called them. */
    val displayName: String = "",
    /** Their photographs, newest capture first. */
    val photos: ImmutableList<PersonPhoto> = persistentListOf(),
    /** Current model suggestions across those photographs, most common first. */
    val suggestions: ImmutableList<AiSuggestion> = persistentListOf(),
    /** Whether the rename sheet is up. */
    val renaming: Boolean = false,
    /** Why the load failed, or `null`. */
    val failure: PeopleFailure? = null,
    /** Why the last action failed, or `null`. */
    val actionFailure: PeopleFailure? = null,
)
