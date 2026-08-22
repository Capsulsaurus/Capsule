package com.justin13888.capsule.core.model

/**
 * The kind of place an import source is.
 *
 * Part of a scope's identity alongside the platform and the locator, and the level at which
 * a default destination can be set once ("all screenshots go here") rather than per folder.
 */
enum class SourceKind(
    override val wire: String,
) : WireEnum {
    /** The system camera roll. */
    CameraRoll("camera_roll"),

    /** The screenshots collection. */
    Screenshots("screenshots"),

    /** A collection another app owns. */
    AppCollection("app_collection"),

    /** A folder the user picked once. */
    Folder("folder"),

    /** A directory the client watches for new files. */
    WatchedDir("watched_dir"),

    /** A card or drive that comes and goes. */
    RemovableVolume("removable_volume"),
    ;

    companion object : WireCodec<SourceKind>({ entries })
}

/**
 * The canonical identity of an import source.
 *
 * [locator] must be the platform's *stable* spelling, not whatever the OS handed over. On
 * Android that means the bucket's relative path (`DCIM/Camera`) and never its bucket id,
 * which is a hash of the display name and differs across devices and OS versions — two
 * phones pointed at the same folder must compute the same scope, and a bucket id would give
 * them different ones.
 */
data class ImportScope(
    /** The platform whose locator convention [locator] follows. */
    val platform: Wire<PlatformTag>,
    /** What kind of source it is. */
    val sourceKind: Wire<SourceKind>,
    /** The platform's canonical, reinstall-stable spelling of the source. */
    val locator: String,
    /** The derived `scope_id`, when it has been computed. */
    val scopeId: ScopeId? = null,
)

/**
 * What the planner decided about one candidate file.
 *
 * The two skip reasons are not interchangeable. [SkipDuplicate] means the library already
 * holds these exact bytes, so importing again would create a second entry for one photo;
 * [SkipUnsupported] means nothing here can read the file at all. A UI that showed them the
 * same way would tell a user their photos were "already imported" when they were not.
 */
sealed interface ImportDecision {
    /** Import it. */
    data class Import(
        val destination: AlbumId,
    ) : ImportDecision

    /** These bytes are already in the library. */
    data class SkipDuplicate(
        val existingAssetId: AssetId,
    ) : ImportDecision

    /** No decoder here handles this format. */
    data object SkipUnsupported : ImportDecision

    /** The file could not be read well enough to decide. */
    data class SkipError(
        val detail: String,
    ) : ImportDecision
}

/** One candidate file and what the planner decided about it. */
data class ImportCandidate(
    /** The source path or content identifier, as the platform spells it. */
    val sourceLocator: String,
    /** Bytes on disk. */
    val sizeBytes: Long,
    /** What will happen to it. */
    val decision: ImportDecision,
)

/**
 * The deterministic output of the planner, rendered for confirmation before anything runs.
 *
 * Deterministic is the operative word: the same inputs produce the same plan, so the numbers
 * the user confirms are the numbers that execute. [streamingRecommended] is the one field
 * the pure planner does not set — it comes from a free-space probe at confirmation time,
 * because probing the filesystem is I/O and I/O in the planner would make the plan
 * irreproducible.
 */
data class ImportPlan(
    /** The run's id — UUIDv7. */
    val importId: ImportId,
    /** Candidates that will be imported, each carrying its resolved destination. */
    val added: List<ImportCandidate>,
    /** Candidates the planner will not import, with the reason on each. */
    val skipped: List<ImportCandidate>,
    /**
     * Candidates that need a decision before the run can proceed.
     *
     * Distinct from [skipped]: a skip is the planner's own conclusion, while a conflict is a
     * question it is handing back.
     */
    val conflicts: List<ImportCandidate>,
    /** The sum of [added] sizes — what the run will actually move. */
    val totalSize: Long,
    /** Which rule produced the destination, recorded so a surprising one is explainable. */
    val destinationRule: DefaultAlbumRule? = null,
    /**
     * Whether the free-space probe says this run should stream.
     *
     * `null` until confirmation attaches it.
     */
    val streamingRecommended: Boolean? = null,
) {
    /** Whether the plan can run without further input. */
    val isReady: Boolean get() = conflicts.isEmpty() && added.isNotEmpty()
}

/**
 * Where one file has got to during the run.
 *
 * A progress vocabulary, deliberately coarse: it exists so the UI can say something true
 * while a long import proceeds, not so a caller can reason about the pipeline's internals.
 */
enum class ImportFileState(
    override val wire: String,
) : WireEnum {
    /** Accepted by the plan, not started. */
    Queued("queued"),

    /** Being read and its metadata extracted. */
    Processing("processing"),

    /** Being encrypted and its manifest signed. */
    Encrypting("encrypting"),

    /** Its bundle is uploading. */
    Uploading("uploading"),

    /** Finished. */
    Done("done"),

    /** Stopped, with a reason. */
    Failed("failed"),
    ;

    /** Whether the file has stopped moving, one way or the other. */
    val isTerminal: Boolean get() = this == Done || this == Failed

    companion object : WireCodec<ImportFileState>({ entries })
}

/**
 * How one file's import ended.
 *
 * Richer than [ImportFileState] on purpose: a state says where a file is, an outcome says
 * what happened to it, and the failures are worth telling apart. In particular
 * [CorruptTransfer] and [CorruptUnreadable] are different problems — one means retry, the
 * other means the source file itself is damaged and retrying will not help.
 */
sealed interface ImportOutcome {
    /** Imported successfully. */
    data class Imported(
        val assetId: AssetId,
    ) : ImportOutcome

    /** These bytes were already in the library. */
    data class DuplicateSkipped(
        val existingAssetId: AssetId,
    ) : ImportOutcome

    /** No decoder here handles this format. */
    data object Unsupported : ImportOutcome

    /** The source file is damaged; retrying will not help. */
    data class CorruptUnreadable(
        val detail: String,
    ) : ImportOutcome

    /** The bytes arrived wrong; the source is probably fine and a retry may succeed. */
    data object CorruptTransfer : ImportOutcome

    /** The OS refused access to the source. */
    data class PermissionDenied(
        val detail: String,
    ) : ImportOutcome

    /** Some members of a stack imported and others did not. */
    data class PartialStackImported(
        /** The members that landed. */
        val imported: List<AssetId>,
        /** The source locators that did not. */
        val skipped: List<String>,
    ) : ImportOutcome

    /** A Live Photo arrived without the other half of the pair. */
    data object LivePhotoWithoutPair : ImportOutcome

    /** Whether this outcome added an asset to the library. */
    val isSuccess: Boolean
        get() = this is Imported || this is PartialStackImported
}

/** Where a whole import run has got to. */
data class ImportProgress(
    /** The run. */
    val importId: ImportId,
    /** Candidates finished, successfully or not. */
    val completed: Int,
    /** Candidates the plan admitted. */
    val total: Int,
    /** The file currently being worked on, when one is. */
    val currentLocator: String? = null,
    /** That file's state. */
    val currentState: ImportFileState? = null,
    /** Terminal outcomes so far, newest last. */
    val outcomes: List<ImportOutcome> = emptyList(),
    /**
     * Set when the run stopped short of its plan.
     *
     * A run that pauses — connectivity lost, quota exhausted mid-stream — is not a failure
     * and leaves this null: the remainder simply waits for the next window, because there is
     * no "must finish" state by construction.
     */
    val failure: ErrorCode? = null,
) {
    /** Whether every admitted candidate has reached a terminal state. */
    val isFinished: Boolean get() = completed >= total
}
