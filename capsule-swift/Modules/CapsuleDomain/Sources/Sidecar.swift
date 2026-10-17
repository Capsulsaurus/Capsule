import CapsuleFoundation
import Foundation

// MARK: - Dimensions

/// Pixel dimensions of an asset.
public struct Dimensions: Sendable, Equatable, Hashable {
    public var width: UInt32
    public var height: UInt32

    public init(width: UInt32, height: UInt32) {
        self.width = width
        self.height = height
    }

    /// Width ÷ height, for grid layout. `nil` for a degenerate zero dimension
    /// rather than a crash or an infinity a layout would propagate.
    public var aspectRatio: Double? {
        guard height > 0, width > 0 else { return nil }
        return Double(width) / Double(height)
    }
}

// MARK: - Lqip

/// The low-quality image placeholder embedded in the encrypted sidecar
/// (*Thumbnails — LQIP*).
///
/// It rides inside the metadata blob, so it is available the instant metadata
/// syncs, at zero extra request — the first rung of the degrade ladder and the
/// only representation guaranteed to be present for every asset the user can
/// see. That is why a missing thumbnail is never a blank tile.
public struct Lqip: Sendable, Equatable, Hashable {
    /// Chromahash bytes, opaque to this layer — decoding belongs to the image
    /// pipeline, which owns the format.
    public var chromahash: Data
    /// The LQIP format version, so a newer encoding is recognised rather than
    /// mis-decoded.
    public var formatVersion: UInt16
    /// Dominant colour, RGB. The zeroth rung of the ladder: renderable with no
    /// decode at all.
    public var dominantColor: RGBColor

    public init(chromahash: Data, formatVersion: UInt16, dominantColor: RGBColor) {
        self.chromahash = chromahash
        self.formatVersion = formatVersion
        self.dominantColor = dominantColor
    }
}

/// An 8-bit-per-channel RGB triple, mirroring the sidecar's `[u8; 3]`.
///
/// Deliberately not a `UIColor`/`NSColor`: this layer imports no UI framework,
/// and the value must survive a round-trip byte-for-byte.
public struct RGBColor: Sendable, Equatable, Hashable {
    public var red: UInt8
    public var green: UInt8
    public var blue: UInt8

    public init(red: UInt8, green: UInt8, blue: UInt8) {
        self.red = red
        self.green = green
        self.blue = blue
    }
}

// MARK: - Identifying fields

/// The camera that took the shot (*Metadata — Identifiers*).
///
/// Fingerprinting surface: the serial uniquely links every photo to one
/// physical body, so it is **stripped by default on export** and retained only
/// on per-export opt-in.
public struct CameraID: Sendable, Equatable, Hashable {
    public var model: String
    public var serial: String

    public init(model: String, serial: String) {
        self.model = model
        self.serial = serial
    }
}

/// An AI-suggested tag, carrying the model that produced it
/// (*Metadata — Tag Provenance and Namespacing*).
///
/// AI tags live in a **structurally separate** OR-set from user tags, so a
/// hallucinating model cannot overwrite user intent — the question does not
/// arise, because they are different fields. The model slot is required
/// because cross-model semantic comparison is forbidden: when the canonical
/// model for a slot changes, the old tags are stale, not wrong.
public struct AiTag: Sendable, Equatable, Hashable, Comparable {
    public var tag: String
    public var modelID: String
    public var modelVersion: String

    public init(tag: String, modelID: String, modelVersion: String) {
        self.tag = tag
        self.modelID = modelID
        self.modelVersion = modelVersion
    }

    /// The `(model_id, model_version)` slot this tag was produced in. A
    /// predicate term over `tags_ai` names a slot; a tag from a different slot
    /// evaluates as stale-excluded rather than being compared across versions.
    public var modelSlot: ModelSlot {
        ModelSlot(modelID: modelID, modelVersion: modelVersion)
    }

    public static func < (lhs: AiTag, rhs: AiTag) -> Bool {
        (lhs.tag, lhs.modelID, lhs.modelVersion) < (rhs.tag, rhs.modelID, rhs.modelVersion)
    }
}

/// A `(model_id, model_version)` pair — the unit AI-derived output is scoped to
/// (*AI — Embedding Provenance*).
public struct ModelSlot: Sendable, Equatable, Hashable {
    public var modelID: String
    public var modelVersion: String

    public init(modelID: String, modelVersion: String) {
        self.modelID = modelID
        self.modelVersion = modelVersion
    }
}

// MARK: - SidecarV1

/// The CBOR sidecar schema v1 — the canonical, plaintext-local-only, **signed**
/// metadata record for one asset (*Metadata — Sidecar Schema v1*).
///
/// A structural mirror of the Rust `capsule_core::sidecar::SidecarV1`, field for
/// field, because these bytes are what the signed manifest and the content hash
/// commit to. A field this layer renames, reorders, or drops is a signature that
/// does not verify on another platform.
///
/// Three properties make it safe to hold in a Swift value type:
///
/// - ``sidecarSchema`` is CBOR field 0, so a reader detects a schema it does not
///   implement *before* parsing the rest. A client whose maximum known schema is
///   below this one **refuses to write** — see ``isWritableBy(maxKnownSchema:)``.
/// - ``unknownFields`` round-trips **verbatim** and is never inspected. The
///   signature covers it, so stripping it invalidates the signature and is
///   detectable.
/// - Every collaborative field is a CRDT (``Lww``, ``OrSet``), so concurrent
///   edits from different devices converge without a conflict dialog.
public struct SidecarV1: Sendable, Equatable, Identifiable {
    /// The schema version this build knows how to write.
    public static let currentSchema: UInt16 = 1

    /// **CBOR field 0.** Readable before the rest of the document is parsed.
    public var sidecarSchema: UInt16
    /// The primitive bundle this sidecar was written under; matches the asset's
    /// manifest.
    public var cryptoSuiteID: UInt16
    /// The asset's canonical identity (UUIDv7) — the same value as the
    /// manifest's `file_id` and the index's `asset_id`, minted once at import.
    public var uuid: String
    /// Canonical **plaintext** digest, lowercase hex. Length fixed by
    /// ``cryptoSuiteID``.
    public var hash: String
    /// RFC 3339 capture time.
    public var captureTimestamp: CapsuleTimestamp
    /// RFC 3339 import time.
    public var importTimestamp: CapsuleTimestamp
    /// The asset's format.
    public var contentType: ContentType
    /// Pixel dimensions, when known.
    public var dimensions: Dimensions?
    /// The embedded display placeholder.
    public var lqip: Lqip?
    /// User tags — an OR-set, structurally separate from ``tagsAI``.
    public var tagsUser: OrSet<String>
    /// AI-suggested tags — a separate OR-set that can never overwrite a user tag.
    public var tagsAI: OrSet<AiTag>
    /// The caption register. Its ``Lww/superseded`` log is what the viewer
    /// surfaces as "this caption replaced another".
    public var caption: Lww<String>
    /// The star rating, 0–5. Orthogonal to ``cull``: a reject can carry three
    /// stars, and tools that conflate them force lossy workflows.
    public var rating: Lww<UInt8>
    /// Stack grouping. An LWW register over an **optional** membership, so
    /// joining, moving between, and leaving a stack are all the same write —
    /// leaving is a stamped `nil`, which is distinct from never having been
    /// written.
    public var stackMembership: Lww<StackMembership?>
    /// The trinary culling flag. Wire-absent default is ``CullFlag/neutral``.
    public var cull: Lww<CullFlag>
    /// The **user-hidden** flag. Wire-absent default is visible.
    ///
    /// One of three distinct flags — see ``LibraryAsset`` for why hidden, trash,
    /// and stack-collapse must never be conflated.
    public var hidden: Lww<Bool>
    /// The capturing camera. Export-stripped by default.
    public var cameraID: CameraID?
    /// The importing device (UUIDv4). Export-stripped by default.
    public var deviceID: DeviceID
    /// The session the import happened in (UUIDv7). Export-stripped by default.
    public var sessionID: SessionID
    /// Geolocation. Export-rounded to ~1 km by default.
    public var gps: Gps?
    /// The **prior** provenance chain head — the record *preceding* the write
    /// that sealed this sidecar (*Metadata — Provenance Binding and Sealing
    /// Order*).
    ///
    /// Deliberately not "the latest record": the latest record *is* the sealing
    /// write, whose manifest commits to this sidecar's hash, so referencing it
    /// would be a cycle. Absent exactly on the initial `create`. It must equal
    /// the sealing manifest's `prior_provenance_hash`; a divergence is
    /// quarantined.
    public var provenanceChainHash: String?
    /// Unknown CBOR keys, preserved **verbatim** and **never inspected**.
    ///
    /// Opaque bytes on purpose: the moment this layer parses them it acquires an
    /// opinion about a schema it does not implement, and the round-trip stops
    /// being byte-exact. The signature covers these bytes.
    public var unknownFields: Data

    public var id: String { uuid }

    public init(
        sidecarSchema: UInt16 = SidecarV1.currentSchema,
        cryptoSuiteID: UInt16,
        uuid: String,
        hash: String,
        captureTimestamp: CapsuleTimestamp,
        importTimestamp: CapsuleTimestamp,
        contentType: ContentType,
        dimensions: Dimensions? = nil,
        lqip: Lqip? = nil,
        tagsUser: OrSet<String> = OrSet(),
        tagsAI: OrSet<AiTag> = OrSet(),
        caption: Lww<String> = Lww(),
        rating: Lww<UInt8> = Lww(),
        stackMembership: Lww<StackMembership?> = Lww(),
        cull: Lww<CullFlag> = Lww(),
        hidden: Lww<Bool> = Lww(),
        cameraID: CameraID? = nil,
        deviceID: DeviceID,
        sessionID: SessionID,
        gps: Gps? = nil,
        provenanceChainHash: String? = nil,
        unknownFields: Data = Data()
    ) {
        self.sidecarSchema = sidecarSchema
        self.cryptoSuiteID = cryptoSuiteID
        self.uuid = uuid
        self.hash = hash
        self.captureTimestamp = captureTimestamp
        self.importTimestamp = importTimestamp
        self.contentType = contentType
        self.dimensions = dimensions
        self.lqip = lqip
        self.tagsUser = tagsUser
        self.tagsAI = tagsAI
        self.caption = caption
        self.rating = rating
        self.stackMembership = stackMembership
        self.cull = cull
        self.hidden = hidden
        self.cameraID = cameraID
        self.deviceID = deviceID
        self.sessionID = sessionID
        self.gps = gps
        self.provenanceChainHash = provenanceChainHash
        self.unknownFields = unknownFields
    }
}

public extension SidecarV1 {
    /// The displaced captions the viewer offers to restore, newest first,
    /// capped at 16 (*Metadata — Surfacing Concurrent Edits*).
    ///
    /// A passthrough of ``caption``'s superseded log rather than a second
    /// stored field: two sources of truth for "what was replaced" is exactly
    /// the divergence the CRDT is there to avoid.
    var supersededCaptions: [Stamped<String>] {
        caption.superseded
    }

    /// The effective culling flag — ``CullFlag/neutral`` when never flagged.
    var cullFlag: CullFlag {
        cull.value ?? .neutral
    }

    /// Whether the user has hidden this asset. `false` when never written.
    var isUserHidden: Bool {
        hidden.value ?? false
    }

    /// The current stack membership, or `nil` for an unstacked asset — whether
    /// it was never stacked or explicitly left a stack.
    var currentStackMembership: StackMembership? {
        stackMembership.value.flatMap(\.self)
    }

    /// Whether a client with the given maximum known schema may **write** this
    /// sidecar (*Metadata — Schema Versioning Rules*).
    ///
    /// The refuse-by-default rule: an old client must not strip-and-resign a
    /// newer sidecar. Reading is still allowed, in explicitly opted-in
    /// read-only mode.
    func isWritableBy(maxKnownSchema: UInt16) -> Bool {
        maxKnownSchema >= sidecarSchema
    }

    /// Whether this sidecar was written by a newer client than this build —
    /// the signal behind the "created with a newer version of Capsule"
    /// indicator.
    var isFromNewerSchema: Bool {
        sidecarSchema > SidecarV1.currentSchema
    }
}
