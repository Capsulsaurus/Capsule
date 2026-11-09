import Foundation

// MARK: - RepresentationTier

/// A rung of the representation ladder, by name (*Download and
/// Synchronization — Tiered, On-Demand Fetch*).
///
/// This type **names** a tier and nothing more: what a port fetches, what
/// storage is spent on, what a sync state reports as the best in hand. It
/// carries no order. Which rung a surface shows, and what it obtains next, is
/// the render ladder's answer (``RenderLadder``), computed once in Rust and
/// reached over the FFI; an app keeps no ladder of its own.
///
/// The cases are declared cheapest first, which is the order a list of them is
/// presented in. The raw values are those the type has always encoded with, so
/// a persisted tier still decodes; they are not an ordering either.
///
/// There is no dominant-colour case. The LQIP rung paints its dominant colour
/// where the LQIP does not decode (*Thumbnails — LQIP*), so the colour is how
/// that rung renders, not a rung of its own. Like the wire enums' closed
/// counterparts it carries no `unknown` case: it never crosses the FFI as a
/// string.
public enum RepresentationTier: Int, Sendable, Equatable, Hashable, CaseIterable, Codable {
    /// The LQIP placeholder, embedded in the metadata blob at zero extra
    /// request, or its dominant colour where it does not decode.
    case lqip = 1
    /// A grid thumbnail.
    case thumbnail = 2
    /// A screen-resolution derivative, for the single-asset view.
    case preview = 3
    /// The full-fidelity original, fetched only on explicit demand: viewing at
    /// full fidelity, exporting, or sharing the original.
    case original = 4
}

// MARK: - SyncScope

/// The per-library fetch policy (*Download and Synchronization —
/// Synchronization Scope*).
///
/// Anything above the configured tier is fetched lazily, on demand; the
/// original is never fetched speculatively unless this device was its uploader,
/// in which case it already holds the bytes.
public enum SyncScope: ClosedWireEnum {
    /// Metadata only — which includes the LQIP, so the timeline is never blank.
    case metadataOnly
    /// Metadata plus thumbnails.
    case metadataAndThumbnails
    /// Metadata, thumbnails, and originals.
    case metadataThumbnailsAndOriginals
    case unknown(String)

    public static let knownCases: [SyncScope] = [
        .metadataOnly, .metadataAndThumbnails, .metadataThumbnailsAndOriginals,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .metadataOnly: "metadata_only"
        case .metadataAndThumbnails: "metadata_and_thumbnails"
        case .metadataThumbnailsAndOriginals: "metadata_thumbnails_and_originals"
        case let .unknown(raw): raw
        }
    }

    /// The highest tier fetched eagerly under this scope. `nil` for an unknown
    /// scope — a newer policy this build must not guess at, so it fetches
    /// nothing eagerly rather than over-fetching on a metered plan.
    public var eagerTier: RepresentationTier? {
        switch self {
        case .metadataOnly: .lqip
        case .metadataAndThumbnails: .thumbnail
        case .metadataThumbnailsAndOriginals: .original
        case .unknown: nil
        }
    }
}

// MARK: - LocalRepresentations

/// Which representations this device actually holds for one asset.
///
/// A set, not a ladder: what to draw is the render ladder's answer over
/// ``renderHoldings(embeddedPreview:)``, and the viewer reads
/// ``isFullResolutionAvailable`` to decide whether to offer a full-fidelity
/// view or a "fetching original" affordance. Neither ever asks the network.
public struct LocalRepresentations: Sendable, Equatable, Hashable {
    /// The tiers held locally.
    public private(set) var heldTiers: Set<RepresentationTier>

    public init(heldTiers: Set<RepresentationTier> = []) {
        self.heldTiers = heldTiers
    }

    /// Whether the original is held, so a full-fidelity view can be shown
    /// without a fetch.
    public var isFullResolutionAvailable: Bool {
        heldTiers.contains(.original)
    }

    /// Whether the given tier can be satisfied locally.
    public func holds(_ tier: RepresentationTier) -> Bool {
        heldTiers.contains(tier)
    }

    /// The result of a successful fetch.
    public func adding(_ tier: RepresentationTier) -> LocalRepresentations {
        LocalRepresentations(heldTiers: heldTiers.union([tier]))
    }

    /// The result of cache eviction. Evicting is always safe for a
    /// server-origin blob — it came from the server, so it can be re-fetched.
    /// A *device-owned* original is a different matter and is gated by
    /// verify-before-destroy, which is a port concern, not a value-type one.
    public func removing(_ tier: RepresentationTier) -> LocalRepresentations {
        LocalRepresentations(heldTiers: heldTiers.subtracting([tier]))
    }

    /// What the render ladder is told this device holds: the encoded tiers and
    /// the original, and the size of the original's embedded preview where one
    /// was read. The LQIP is implied there, so it is not listed.
    public func renderHoldings(embeddedPreview: Dimensions? = nil) -> RenderHoldings {
        RenderHoldings(held: heldTiers.subtracting([.lqip]), embeddedPreview: embeddedPreview)
    }
}

// MARK: - UnreadableReason

/// Why an asset cannot be read on *this* device, specifically.
///
/// Separate from ``RejectReason`` on purpose: nothing here implies the asset is
/// invalid. It is valid and this device simply cannot open it, which is a
/// materially different thing to tell a user.
public enum UnreadableReason: Sendable, Equatable, Hashable {
    /// The album key for the asset's epoch has not been delivered to this
    /// device yet.
    case albumKeyNotDelivered
    /// This build has no codec for the asset's format.
    case noCodecForContentType(ContentType)
    /// The local bytes failed their integrity check and were discarded pending
    /// a re-fetch.
    case localBytesCorrupt
    /// The asset lives in an album whose upgrade ceremony has not completed on
    /// this device.
    case albumUpgradePending
}

// MARK: - SchemaAhead

/// A document written against a schema this build does not implement.
///
/// Carried rather than flattened to a boolean so the UI can say *which* surface
/// is ahead — a sidecar, a smart-album predicate, a sync feed entry — and so a
/// diagnostic report names the exact version pair.
public struct SchemaAhead: Sendable, Equatable, Hashable {
    /// Which versioned surface is ahead.
    public enum Surface: Sendable, Equatable, Hashable {
        case sidecarSchema
        case predicateSchema
        case settingsSchema
        case protocolVersion
    }

    public var surface: Surface
    /// The version found in the document.
    public var found: String
    /// The maximum this build knows.
    public var maxKnown: String

    public init(surface: Surface, found: String, maxKnown: String) {
        self.surface = surface
        self.found = found
        self.maxKnown = maxKnown
    }
}

// MARK: - AssetSyncState

/// Where one asset stands between this device and its home server.
///
/// The states are mutually exclusive and each drives a distinct, honest UI
/// affordance. In particular ``awaitingOriginal(heldBy:)`` is a **badge, never a
/// failure**: the asset is visible on every device the moment its manifest and
/// metadata finalize, and its original may legitimately still be sitting on the
/// phone that took it under a staged upload policy. Rendering that as an error
/// would train users to distrust a working system.
public enum AssetSyncState: Sendable, Equatable, Hashable {
    /// A transfer is in flight for this asset.
    case uploading(tier: UploadTier, transferred: UInt64, total: UInt64)
    /// Visible everywhere, but the original has not landed on the server yet —
    /// it is still on another device. Fetching it yields the *transient*
    /// `error.blob.pending_upload`, explicitly distinct from `410 Gone`.
    case awaitingOriginal(heldBy: DeviceID?)
    /// Confirmed stored, indexed, and retrievable on the home server. Only in
    /// this state may an irreplaceable local copy be released.
    case durable
    /// Held for a human decision; never applied, never dropped.
    case quarantined(QuarantineID)
    /// Valid, but this device cannot open it.
    case unreadableOnThisDevice(UnreadableReason)
    /// Written against a newer schema. Preserved verbatim through sync
    /// round-trips and **never stripped**; surfaced as "created with a newer
    /// version of Capsule".
    case writtenByNewerVersion(SchemaAhead)
    /// A higher rung is permanently unavailable — a purged origin, an
    /// unreachable federated home server — so the asset renders at the best
    /// representation in hand. Non-destructive: metadata and the index entry
    /// stay, and the asset re-fetches automatically once reachable again.
    case fullResolutionUnavailable(bestAvailable: RepresentationTier)

    /// Whether this state needs the user's attention, as opposed to resolving
    /// on its own. Drives whether a badge is informational or actionable.
    public var needsUserAttention: Bool {
        switch self {
        case .quarantined, .unreadableOnThisDevice, .writtenByNewerVersion: true
        case .uploading, .awaitingOriginal, .durable, .fullResolutionUnavailable: false
        }
    }

    /// Whether this device may release its only local copy of the asset's
    /// bytes. **Only** `durable` qualifies — the verify-before-destroy gate.
    public var permitsLocalRelease: Bool {
        self == .durable
    }
}
