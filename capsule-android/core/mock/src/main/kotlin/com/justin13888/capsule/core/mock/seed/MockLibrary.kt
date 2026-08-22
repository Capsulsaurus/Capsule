package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.QuarantineEntry
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UserId

/**
 * The one asset per documented state, found by name.
 *
 * Without this, "the library contains a quarantined asset somewhere" is only assertable by
 * scanning forty thousand records for one, and reachable from the UI only by scrolling until
 * one appears. Naming them turns the state matrix into something a test can check exhaustively
 * and a debug drawer can jump to, which is the difference between coverage that is maintained
 * and coverage that quietly rots.
 *
 * Every asset-shaped landmark is nullable because an empty library is a legitimate fixture —
 * the first-run state has no assets to be hidden or trashed. Nullable and asserted non-null in
 * the tests that use a populated spec beats a sentinel id that would look like a real asset
 * everywhere it leaked to.
 */
data class LibraryLandmarks(
    /** The nameless container every unfiled import lands in. */
    val defaultAlbumId: AlbumId,
    /** The trip albums, oldest first. */
    val tripAlbumIds: List<AlbumId>,
    /** A key-free view album, which is never an import destination. */
    val viewAlbumId: AlbumId,
    /** An asset the user hid — present in its album, absent from every default view. */
    val hiddenAssetId: AssetId?,
    /** Trashed within the last day; most of its retention window remains. */
    val trashedFreshAssetId: AssetId?,
    /** Trashed around the middle of its window. */
    val trashedMidWindowAssetId: AssetId?,
    /** Trashed past its signed `retention_until`, so a purge worker may now proceed. */
    val trashedExpiredAssetId: AssetId?,
    /** Visible everywhere, but its original has not left the capturing device yet. */
    val awaitingOriginalAssetId: AssetId?,
    /** Holds only the placeholder, so any above-tier fetch degrades. */
    val degradedAssetId: AssetId?,
    /** Inbound state was refused and is waiting for a human decision. */
    val quarantinedAssetId: AssetId?,
    /** Written by a newer `protocol_version`: its enum tokens are outside this build's sets. */
    val forwardSchemaAssetId: AssetId?,
    /** A format no decoder on this device handles. */
    val unreadableAssetId: AssetId?,
    /** Its stored fix is in GCJ-02, so plotting it as WGS-84 puts it in the wrong place. */
    val gcj02AssetId: AssetId?,
    /** A RAW+JPEG pair. */
    val rawJpegStackId: StackId?,
    /** A burst with a chosen primary. */
    val burstStackId: StackId?,
    /** A still and its motion clip. */
    val livePhotoStackId: StackId?,
    /** One long recording the camera split at a file-size limit. */
    val chapteredStackId: StackId?,
    /** A frame wide enough to clamp at the aspect codec's ceiling. */
    val panoramaAssetId: AssetId?,
    /** A video. */
    val videoAssetId: AssetId?,
    /** A square frame. */
    val squareAssetId: AssetId?,
    /** Carries an AI tag from a model version that has since been superseded. */
    val staleAiTagAssetId: AssetId?,
    /** A face cluster whose model slot is no longer canonical. */
    val staleClusterId: FaceClusterId?,
    /** The device this process is notionally running on. */
    val thisDeviceId: DeviceId,
    /** The account everything belongs to. */
    val ownerId: UserId,
)

/**
 * A complete generated library: every record the sixteen repository contracts can hand out.
 *
 * Immutable by construction. The mutable half of the mock — the writes a user makes while the
 * app is running — lives in the backend that wraps this, so the generated fixture can be
 * rebuilt or compared without any of a session's edits leaking into it.
 */
data class MockLibrary(
    /** The spec this library was generated from. */
    val spec: LibrarySpec,
    /** Every asset, newest capture first — the timeline's own ordering. */
    val assets: List<Asset>,
    /** The EXIF beside each asset, which the catalog row does not project. */
    val exif: Map<AssetId, MockExif>,
    /** Container and view albums. */
    val albums: List<Album>,
    /** Every stack. */
    val stacks: List<Stack>,
    /** Each stack's members, in sequence order. */
    val stackMembers: Map<StackId, List<StackMember>>,
    /** Captions, for the assets that have one. */
    val captions: Map<AssetId, Caption>,
    /** Both tag namespaces, for the assets that have any. */
    val tags: Map<AssetId, AssetTags>,
    /** Each asset's provenance chain, oldest first. */
    val provenance: Map<AssetId, List<ProvenanceRecord>>,
    /** The people the user named. */
    val people: List<Person>,
    /** The current face clusters, stale slots included. */
    val clusters: List<FaceCluster>,
    /** Saved searches, including one this build cannot evaluate. */
    val smartAlbums: List<SmartAlbumDefinition>,
    /** Items held for a human decision. */
    val quarantine: List<QuarantineEntry>,
    /** The signed device directory, revoked devices included. */
    val devices: List<Device>,
    /** Sessions on the account. */
    val sessions: List<Session>,
    /** Sessions grouped by the physical device they appear to come from. */
    val cohorts: List<DeviceCohort>,
    /** Issued read links. */
    val shareLinks: List<ShareLink>,
    /** Provisioned upload links. */
    val uploadLinks: List<UploadLink>,
    /** Files guests deposited and the user has not yet reviewed. */
    val drops: List<PendingDrop>,
    /** Operator actions recorded against the account. */
    val moderationEvents: List<ModerationEvent>,
    /** Import sources this device has seen. */
    val scopes: List<ImportScope>,
    /** What the server accounts to this uploader. */
    val quota: QuotaStatus,
    /** The named asset per documented state. */
    val landmarks: LibraryLandmarks,
) {
    /** Every asset by id, in timeline order — the lookup every repository read starts from. */
    val assetsById: Map<AssetId, Asset> = assets.associateByTo(LinkedHashMap(), Asset::id)
}
