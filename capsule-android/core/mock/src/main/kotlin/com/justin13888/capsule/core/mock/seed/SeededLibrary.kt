package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.QuarantineEntry
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UserId
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * Builds a plausible five-year professional photo library from a single seed.
 *
 * The whole point is that "plausible" and "reproducible" are not in tension here. Every draw
 * descends from [LibrarySpec.seed] through a [Prng], and every collection is built in a fixed
 * order, so two runs — in the same process or a year apart on another machine — produce
 * byte-identical output. That is what makes a screenshot golden of a gallery meaningful: a
 * diff in the image is a diff in the *renderer*, never in the fixture underneath it.
 */
object SeededLibrary {
    /** Generates the library [spec] describes. Pure: no clock, no filesystem, no network. */
    fun generate(spec: LibrarySpec = LibrarySpec.DEFAULT): MockLibrary = LibraryGenerator(spec).build()
}

/**
 * The generator's working state, threaded through the synthesis passes as a receiver.
 *
 * A mutable class rather than a chain of pure functions returning ever-larger tuples: the
 * passes genuinely share state — an asset needs an album id, a stack needs asset ids, the
 * state matrix rewrites assets the frame pass produced — and expressing that as parameter
 * passing would produce a twenty-argument signature that hides rather than reveals the
 * dependencies.
 *
 * Each pass draws from its **own named sub-stream**. Adding a field to the EXIF profile then
 * cannot shift the capture times or the ids, so an edit to one pass leaves every other pass's
 * output untouched — and only the goldens that actually changed need recapturing.
 */
@Suppress("TooManyFunctions")
internal class LibraryGenerator(
    val spec: LibrarySpec,
) {
    private val root = Prng(spec.seed)

    /** Identifier minting. */
    val idPrng: Prng = root.derive("ids")

    /** Frame composition — cameras, scenes, aspect ratios, capture times. */
    val framePrng: Prng = root.derive("frames")

    /** Exposure triples. */
    val exifPrng: Prng = root.derive("exif")

    /** Track sampling and jitter. */
    val gpsPrng: Prng = root.derive("gps")

    /** Stack shapes and their primaries. */
    val stackPrng: Prng = root.derive("stacks")

    /** Captions, tags, people, saved searches. */
    val metadataPrng: Prng = root.derive("metadata")

    /** Devices, sessions, links, quota. */
    val accountPrng: Prng = root.derive("account")

    /** Placeholder chroma grids. */
    val lqipPrng: Prng = root.derive("lqip")

    private var ownerOrNull: UserId? = null

    /**
     * The account everything belongs to.
     *
     * Backed by a nullable field rather than `lateinit`, which Kotlin does not allow on an
     * inline value class. The accessor still fails loudly if a pass reads it before the
     * account pass has run, which is the only thing `lateinit` was buying.
     */
    var owner: UserId
        get() = requireNotNull(ownerOrNull) { "the account pass has not run yet" }
        set(value) {
            ownerOrNull = value
        }

    /** The device this process is notionally running on — the writer of every local stamp. */
    lateinit var thisDevice: Device

    private var importSessionOrNull: SessionId? = null

    /** The session imports are attributed to. Value-class-backed for the same reason. */
    var importSession: SessionId
        get() = requireNotNull(importSessionOrNull) { "the account pass has not run yet" }
        set(value) {
            importSessionOrNull = value
        }

    /** The signed device directory. */
    val devices: MutableList<Device> = mutableListOf()

    /** Sessions on the account. */
    val sessions: MutableList<Session> = mutableListOf()

    /** Session groupings by physical device. */
    val cohorts: MutableList<DeviceCohort> = mutableListOf()

    /** The nameless container that receives every unfiled import. */
    lateinit var defaultAlbum: Album

    /** Container and view albums, in display order. */
    val albums: MutableList<Album> = mutableListOf()

    /** Trip index to the album that trip produced. */
    val tripAlbums: MutableMap<Int, Album> = LinkedHashMap()

    /** Every asset, oldest first while building and reversed once. */
    val assets: MutableList<Asset> = mutableListOf()

    /** Where each asset sits in [assets], so a later pass can rewrite it in place. */
    val assetIndex: MutableMap<AssetId, Int> = LinkedHashMap()

    /** The EXIF beside each asset. */
    val exif: MutableMap<AssetId, MockExif> = LinkedHashMap()

    /** Every stack. */
    val stacks: MutableList<Stack> = mutableListOf()

    /** Each stack's members. */
    val stackMembers: MutableMap<StackId, MutableList<StackMember>> = LinkedHashMap()

    /** Captions, for the assets that have one. */
    val captions: MutableMap<AssetId, Caption> = LinkedHashMap()

    /** Both tag namespaces. */
    val tags: MutableMap<AssetId, AssetTags> = LinkedHashMap()

    /** Provenance chains. */
    val provenance: MutableMap<AssetId, List<ProvenanceRecord>> = LinkedHashMap()

    /** Named people. */
    val people: MutableList<Person> = mutableListOf()

    /** Face clusters. */
    val clusters: MutableList<FaceCluster> = mutableListOf()

    /** Saved searches. */
    val smartAlbums: MutableList<SmartAlbumDefinition> = mutableListOf()

    /** Items held for a human decision. */
    val quarantine: MutableList<QuarantineEntry> = mutableListOf()

    /** Issued read links. */
    val shareLinks: MutableList<ShareLink> = mutableListOf()

    /** Provisioned upload links. */
    val uploadLinks: MutableList<UploadLink> = mutableListOf()

    /** Unreviewed guest deposits. */
    val drops: MutableList<PendingDrop> = mutableListOf()

    /** Operator actions against the account. */
    val moderationEvents: MutableList<ModerationEvent> = mutableListOf()

    /** Import sources this device has seen. */
    val scopes: MutableList<ImportScope> = mutableListOf()

    /** What the server accounts to this uploader. */
    lateinit var quota: QuotaStatus

    /** Strictly increasing per-device OR-set add counter, as the CRDT contract requires. */
    private var addCounter = 0L

    /** The next OR-set add id for a set on [thisDevice]. */
    fun nextAddId(): com.justin13888.capsule.core.model.AddId =
        com.justin13888.capsule.core.model
            .AddId(thisDevice.id, addCounter++)

    /** The library's last day — one past the newest capture. */
    val endExclusive: LocalDate get() =
        spec.now
            .atZone(ZoneOffset.UTC)
            .toLocalDate()
            .plusDays(1)

    /** The library's first day. */
    val start: LocalDate get() = endExclusive.minusYears(spec.years.toLong())

    /** Records an asset, remembering where it landed so a later pass can rewrite it. */
    fun addAsset(asset: Asset) {
        assetIndex[asset.id] = assets.size
        assets += asset
    }

    /** Rewrites an already-generated asset in place. */
    fun mutateAsset(
        id: AssetId,
        transform: (Asset) -> Asset,
    ) {
        val at = assetIndex[id] ?: return
        assets[at] = transform(assets[at])
    }

    /** The asset currently recorded under [id]. */
    fun assetOf(id: AssetId): Asset? = assetIndex[id]?.let(assets::get)

    /**
     * Runs every pass, in the only order they can run in.
     *
     * The account comes first because an asset's stamps name a device; albums come before
     * assets because an asset names its container; the state matrix comes last because it
     * rewrites assets the frame pass has already produced.
     */
    fun build(): MockLibrary {
        synthesizeAccount()
        val days = DayPlanner.plan(start, endExclusive, spec.targetAssetCount, root.derive("plan"))
        val trips = days.mapNotNull(PlannedDay::trip).distinctBy(Trip::index)
        synthesizeAlbums(trips)
        days.forEach(::synthesizeDay)
        synthesizeCollections()
        val landmarks = synthesizeStateMatrix()
        // After the state matrix, not before: it hides and trashes assets, and a count taken
        // earlier would be the number ever filed into an album rather than the number a user
        // sees in it.
        finalizeAlbums()
        synthesizeAccountArtifacts()
        return assemble(landmarks)
    }

    /** Freezes the mutable working state into the immutable fixture, newest capture first. */
    private fun assemble(landmarks: LibraryLandmarks): MockLibrary =
        MockLibrary(
            spec = spec,
            assets = assets.sortedWith(compareByDescending(Asset::captureTimestamp).thenBy { it.id.value }),
            exif = exif.toMap(),
            albums = albums.toList(),
            stacks = stacks.toList(),
            stackMembers = stackMembers.mapValues { (_, members) -> members.toList() },
            captions = captions.toMap(),
            tags = tags.toMap(),
            provenance = provenance.toMap(),
            people = people.toList(),
            clusters = clusters.toList(),
            smartAlbums = smartAlbums.toList(),
            quarantine = quarantine.toList(),
            devices = devices.toList(),
            sessions = sessions.toList(),
            cohorts = cohorts.toList(),
            shareLinks = shareLinks.toList(),
            uploadLinks = uploadLinks.toList(),
            drops = drops.toList(),
            moderationEvents = moderationEvents.toList(),
            scopes = scopes.toList(),
            quota = quota,
            landmarks = landmarks,
        )
}
