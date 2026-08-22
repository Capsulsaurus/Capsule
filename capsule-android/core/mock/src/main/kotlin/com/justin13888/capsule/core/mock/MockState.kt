package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportProgress
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.LibrarySettings
import com.justin13888.capsule.core.model.LocalAuthGrant
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.QuarantineEntry
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import com.justin13888.capsule.core.model.UserBlock
import java.time.Instant

/**
 * The mutable half of the mock — everything a running session can change.
 *
 * Deliberately separate from the immutable [MockLibrary] it is seeded from. A caption written
 * during a screenshot run must not leak into the fixture, and keeping the generated library
 * frozen means it can be regenerated, digested and compared without a session's edits in the
 * way. It also means "reset the app to a known state" is one object allocation rather than a
 * careful undo.
 *
 * Every collection is insertion-ordered. A `HashMap` here would make the timeline's order
 * depend on hash-bucket layout, which is stable within a JVM version and not across them —
 * the precise kind of non-determinism a screenshot golden discovers six months later.
 */
@Suppress("LongParameterList")
class MockState(
    library: MockLibrary,
) {
    /** Every asset by id, newest capture first. */
    val assets: LinkedHashMap<AssetId, Asset> = LinkedHashMap(library.assetsById)

    /** Container and view albums, in display order. */
    val albums: MutableList<Album> = library.albums.toMutableList()

    /** Which container the owner's default pointer names. */
    var defaultAlbumId: AlbumId = library.landmarks.defaultAlbumId

    /** Stacks by id. */
    val stacks: LinkedHashMap<StackId, Stack> = LinkedHashMap(library.stacks.associateBy(Stack::id))

    /** Each stack's members, in sequence order. */
    val stackMembers: LinkedHashMap<StackId, MutableList<StackMember>> =
        LinkedHashMap(library.stackMembers.mapValues { (_, rows) -> rows.toMutableList() })

    /** Captions, keyed by asset. */
    val captions: LinkedHashMap<AssetId, Caption> = LinkedHashMap(library.captions)

    /** Both tag namespaces, keyed by asset. */
    val tags: LinkedHashMap<AssetId, AssetTags> = LinkedHashMap(library.tags)

    /** Provenance chains for the assets that have an interesting history. */
    val provenance: LinkedHashMap<AssetId, List<ProvenanceRecord>> = LinkedHashMap(library.provenance)

    /** Named people. */
    val people: MutableList<Person> = library.people.toMutableList()

    /** Face clusters. */
    val clusters: MutableList<FaceCluster> = library.clusters.toMutableList()

    /** People the user removed from the People view without deleting their clusters. */
    val hiddenPeople: MutableSet<PersonId> = LinkedHashSet()

    /** Saved searches by id. */
    val smartAlbums: LinkedHashMap<SmartAlbumId, SmartAlbumDefinition> =
        LinkedHashMap(library.smartAlbums.associateBy(SmartAlbumDefinition::id))

    /** Issued read links. */
    val shareLinks: LinkedHashMap<ShareLinkId, ShareLink> =
        LinkedHashMap(library.shareLinks.associateBy(ShareLink::id))

    /** Provisioned upload links. */
    val uploadLinks: LinkedHashMap<UploadLinkId, UploadLink> =
        LinkedHashMap(library.uploadLinks.associateBy(UploadLink::id))

    /** Files awaiting review. */
    val drops: LinkedHashMap<com.justin13888.capsule.core.model.DropId, PendingDrop> =
        LinkedHashMap(library.drops.associateBy(PendingDrop::id))

    /** Items held for a human decision. */
    val quarantine: MutableList<QuarantineEntry> = library.quarantine.toMutableList()

    /** Accounts this user has blocked. */
    val blocks: MutableList<UserBlock> = mutableListOf()

    /** Sessions on the account. */
    val sessions: LinkedHashMap<SessionId, Session> = LinkedHashMap(library.sessions.associateBy(Session::id))

    /** The signed device directory. */
    val devices: LinkedHashMap<DeviceId, Device> = LinkedHashMap(library.devices.associateBy(Device::id))

    /** Live local-auth grants, one per gated view. */
    val grants: LinkedHashMap<GatedView, LocalAuthGrant> = LinkedHashMap()

    /** Import sources this device has seen. */
    val scopes: MutableList<ImportScope> = library.scopes.toMutableList()

    /** Where each source's imports are filed. */
    val scopeDestinations: LinkedHashMap<String, AlbumId> = LinkedHashMap()

    /** Progress for every import run started this session. */
    val imports: LinkedHashMap<ImportId, ImportProgress> = LinkedHashMap()

    /** Assets pinned against cache eviction. */
    val pinned: MutableSet<AssetId> = LinkedHashSet()

    /** Clusters the user detached from the person they were attached to. */
    val detachedClusters: MutableSet<FaceClusterId> = LinkedHashSet()

    /** The per-library preferences. */
    var settings: LibrarySettings = LibrarySettings()

    /** Bytes the local blob cache occupies. */
    var cacheBytes: Long = library.assets.size.toLong() * CACHE_BYTES_PER_ASSET

    /** When a reconciliation last completed; `null` until the scenario or a force-sync sets it. */
    var lastCompletedSync: Instant? = null

    /** Strictly increasing OR-set add counter for this device. */
    var addCounter: Long = INITIAL_ADD_COUNTER

    /** Backup artifacts this client knows about. */
    val backups: MutableList<BackupArtifactSummary> = mutableListOf(seedBackup(library))

    /** When the user last proved they still hold their recovery secret. */
    var recoveryLastVerifiedAt: Instant? = library.spec.now.minus(java.time.Duration.ofDays(RECOVERY_VERIFIED_DAYS))

    /** How many verifications have succeeded in a row, which widens the prompt interval. */
    var recoverySuccesses: Int = 1

    /** How many have failed in a row, which eventually recommends a re-wrap. */
    var recoveryFailures: Int = 0

    /** When a snoozed verification prompt returns. */
    var recoverySnoozedUntil: Instant? = null

    /** How many times in a row the prompt has been snoozed. */
    var recoverySnoozes: Int = 0

    /**
     * Replaces an asset in place, keeping the timeline's ordering.
     *
     * Every ordinary write goes through here rather than through a re-sort: a rating change
     * cannot move an asset in a timeline sorted by capture time, and re-sorting forty thousand
     * entries on each keystroke would make culling unusable.
     */
    fun put(asset: Asset) {
        assets[asset.id] = asset
    }

    /**
     * Inserts an asset that did not exist before, restoring the newest-first ordering.
     *
     * Only adoption and import take this path, which is why the rebuild is affordable: they
     * happen once per user action rather than once per frame.
     */
    fun insertOrdered(asset: Asset) {
        val order = compareByDescending(Asset::captureTimestamp).thenBy { asset: Asset -> asset.id.value }
        val merged = (assets.values + asset).sortedWith(order)
        assets.clear()
        merged.forEach { assets[it.id] = it }
    }

    private companion object {
        /** A thumbnail plus a preview per asset, roughly. */
        const val CACHE_BYTES_PER_ASSET = 96L * 1024

        /**
         * Starts well above anything the generator minted, so a session's adds can never alias
         * a generated one — reusing a counter would make removing one tag delete another.
         */
        const val INITIAL_ADD_COUNTER = 1_000_000L

        /** How long ago the recovery secret was last proved, in the seeded state. */
        const val RECOVERY_VERIFIED_DAYS = 40L

        /**
         * The one backup artifact the fixture starts with.
         *
         * Seeded from the library's own seed so its id is stable, and marked authenticated
         * because both checks — the manifest MAC and the exporting device's signature —
         * passed when this client read it.
         */
        fun seedBackup(library: MockLibrary): BackupArtifactSummary {
            val prng = Prng(Prng.mixSeed(library.spec.seed, "backup"))
            return BackupArtifactSummary(
                id = BackupArtifactId(DeterministicIds.uuidV7(library.spec.now.toEpochMilli(), prng)),
                artifactVersion = 1,
                cryptoSuiteId = 1,
                minProtocolVersion = "2026-01-01",
                exporterDevice = library.landmarks.thisDeviceId,
                exportedAt = library.spec.now.minus(java.time.Duration.ofDays(RECOVERY_VERIFIED_DAYS)),
                entryCount = library.assets.size,
                totalBytes = library.quota.used,
                isAuthenticated = true,
            )
        }
    }
}
