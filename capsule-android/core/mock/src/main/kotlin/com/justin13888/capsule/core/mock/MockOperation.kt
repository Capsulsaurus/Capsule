package com.justin13888.capsule.core.mock

/**
 * The unit that latency and failure are injected at.
 *
 * Coarser than one constant per repository method on purpose. A caller wanting "make album
 * writes fail" should not have to enumerate `insertAlbum`, `updateAlbum`, `deleteAlbum` and
 * `setAssetAlbum` and then keep that list up to date as the contract grows; and a caller
 * wanting one specific method to fail can still say so, because failures are injected by
 * operation *and* the scenario decides which operations a condition touches.
 *
 * The two flags are what let one scenario toggle produce the right behaviour everywhere at
 * once. [needsNetwork] is the reason a gallery read keeps working offline while a sync does
 * not — a read path that attempted network I/O would be a bug, not a condition to retry.
 */
enum class MockOperation(
    /** Whether the operation needs a usable network path and an authenticated session. */
    val needsNetwork: Boolean,
    /**
     * Whether the operation grows stored metadata.
     *
     * The distinction quota's grace-expired state turns on: metadata-growth writes are refused
     * there, while reads, deletes and restore-from-trash keep working. A user must be able to
     * delete their way back under quota.
     */
    val growsMetadata: Boolean = false,
) {
    /** A window of the timeline or the trash. */
    TimelineRead(needsNetwork = false),

    /** A single asset lookup or observation. */
    AssetRead(needsNetwork = false),

    /** A rating, cull flag, hidden flag or stored fix. */
    AssetWrite(needsNetwork = false, growsMetadata = true),

    /** Soft-delete, restore, or purge. Never refused for quota — that would be a trap. */
    TrashWrite(needsNetwork = false),

    /** Reading albums and their contents. */
    AlbumRead(needsNetwork = false),

    /** Creating, renaming, deleting an album, or moving an asset between albums. */
    AlbumWrite(needsNetwork = false, growsMetadata = true),

    /** Captions and tags. */
    MetadataWrite(needsNetwork = false, growsMetadata = true),

    /** Stack shape, primary, collapse state. */
    StackWrite(needsNetwork = false, growsMetadata = true),

    /** Local search, and smart-album evaluation. */
    Search(needsNetwork = false),

    /** Saving or deleting a smart-album definition. */
    SmartAlbumWrite(needsNetwork = false, growsMetadata = true),

    /** People and face clusters. */
    PeopleRead(needsNetwork = false),

    /** Naming, merging, and attaching clusters to people. */
    PeopleWrite(needsNetwork = false, growsMetadata = true),

    /** Listing sessions and devices. */
    SessionRead(needsNetwork = true),

    /** Revoking a session or a device. */
    SessionWrite(needsNetwork = true),

    /** The local-auth gate in front of Recently Deleted and Hidden. */
    LocalAuth(needsNetwork = false),

    /** Changing a library preference. */
    SettingsWrite(needsNetwork = false),

    /** Issuing or revoking a share or upload link. */
    LinkWrite(needsNetwork = true, growsMetadata = true),

    /** Adopting or discarding a deposited file. */
    DropWrite(needsNetwork = true, growsMetadata = true),

    /** Planning an import. Pure and local; never refused. */
    ImportPlan(needsNetwork = false),

    /** Running an import, which opens upload sessions. */
    ImportRun(needsNetwork = true, growsMetadata = true),

    /** Blocking a user or filing a report. */
    ModerationWrite(needsNetwork = true, growsMetadata = true),

    /** Verifying a recovery secret, or reading a backup manifest. */
    RecoveryRead(needsNetwork = false),

    /** Exporting or restoring a backup. */
    RecoveryWrite(needsNetwork = true),

    /** Quota, cache occupancy, held representations. */
    StorageRead(needsNetwork = false),

    /** Cache eviction, pinning, and the verify-before-destroy check. */
    StorageWrite(needsNetwork = true),

    /** Force-sync, scope and policy changes. */
    SyncControl(needsNetwork = true),

    /** Fetching a rung of the representation ladder. */
    RepresentationFetch(needsNetwork = true),

    /** Adding a device to the account. */
    Enrollment(needsNetwork = true),
}
