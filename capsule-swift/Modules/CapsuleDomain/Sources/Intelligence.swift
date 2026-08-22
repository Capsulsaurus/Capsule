import CapsuleFoundation
import Foundation

// MARK: - PersonCluster

/// A face cluster produced by on-device grouping (*AI — AI Output
/// Containment*).
///
/// A cluster is **scoped to one model slot**. Comparing clusters across model
/// versions is forbidden, which is why ``modelSlot`` travels with every cluster
/// rather than being a global setting: when the canonical model changes, the old
/// clusters are stale, not wrong, and must be recomputed rather than reconciled.
///
/// The user-assigned name is an LWW register like any other collaborative field,
/// so naming a person on one device converges with naming them on another.
public struct PersonCluster: Sendable, Equatable, Identifiable, Hashable {
    public var id: PersonID
    /// The user-assigned name, when set. Absent for an unnamed cluster — never
    /// a fabricated placeholder.
    public var name: Lww<String>
    /// The asset whose face crop represents the cluster.
    public var keyAssetID: AssetID?
    /// How many assets are in the cluster.
    public var assetCount: Int
    /// The model slot this cluster was produced in.
    public var modelSlot: ModelSlot
    /// Whether the cluster is stale — its slot's canonical model has changed, so
    /// it is **excluded from evaluation until regenerated**, never compared
    /// across versions.
    public var isStale: Bool
    /// Whether the user hid this cluster from the People surface.
    public var isHidden: Bool

    public init(
        id: PersonID,
        name: Lww<String> = Lww(),
        keyAssetID: AssetID? = nil,
        assetCount: Int,
        modelSlot: ModelSlot,
        isStale: Bool = false,
        isHidden: Bool = false
    ) {
        self.id = id
        self.name = name
        self.keyAssetID = keyAssetID
        self.assetCount = assetCount
        self.modelSlot = modelSlot
        self.isStale = isStale
        self.isHidden = isHidden
    }

    /// Whether the cluster has been named by the user.
    public var isNamed: Bool {
        name.value?.isEmpty == false
    }
}

// MARK: - PlaceCluster

/// A geographic grouping of assets, for the Places surface.
///
/// Coordinates are carried in **their stored datum**, never converted at rest.
/// A GCJ-02 cluster shown on a WGS-84 map must be marked approximate — see
/// ``GpsDatum/displaysAsApproximate`` — because the inverse conversion is lossy
/// and pretending otherwise puts a pin in the wrong street.
public struct PlaceCluster: Sendable, Equatable, Identifiable, Hashable {
    /// A stable identity for the cluster within its zoom level.
    public var id: String
    /// The cluster centroid, in its stored datum.
    public var centroid: Gps
    public var assetCount: Int
    /// The asset whose thumbnail represents the cluster.
    public var keyAssetID: AssetID?

    public init(id: String, centroid: Gps, assetCount: Int, keyAssetID: AssetID? = nil) {
        self.id = id
        self.centroid = centroid
        self.assetCount = assetCount
        self.keyAssetID = keyAssetID
    }
}

// MARK: - MapRegion

/// A bounding box for a Places query, in decimal degrees.
///
/// A plain value rather than an `MKCoordinateRegion` so the domain layer stays
/// free of MapKit — the platform-boundary rule this module is on the clean side
/// of.
public struct MapRegion: Sendable, Equatable, Hashable {
    public var minimumLatitude: Double
    public var maximumLatitude: Double
    public var minimumLongitude: Double
    public var maximumLongitude: Double

    public init(
        minimumLatitude: Double,
        maximumLatitude: Double,
        minimumLongitude: Double,
        maximumLongitude: Double
    ) {
        self.minimumLatitude = minimumLatitude
        self.maximumLatitude = maximumLatitude
        self.minimumLongitude = minimumLongitude
        self.maximumLongitude = maximumLongitude
    }
}

// MARK: - SearchScope

/// Which facets a search covers.
public struct SearchScope: Sendable, Equatable, Hashable, OptionSet {
    public let rawValue: Int

    public init(rawValue: Int) {
        self.rawValue = rawValue
    }

    /// User-authored captions and tags.
    public static let userText = SearchScope(rawValue: 1 << 0)
    /// AI-suggested tags, subject to the staleness rule.
    public static let aiTags = SearchScope(rawValue: 1 << 1)
    /// Named people clusters.
    public static let people = SearchScope(rawValue: 1 << 2)
    /// Place names and coordinates.
    public static let places = SearchScope(rawValue: 1 << 3)
    /// Semantic similarity over embeddings.
    public static let semantic = SearchScope(rawValue: 1 << 4)

    /// Everything the build supports.
    public static let all: SearchScope = [.userText, .aiTags, .people, .places, .semantic]
}

// MARK: - SearchResult

/// One search hit.
///
/// ``matchedScope`` is carried so results can be grouped and, more importantly,
/// so a semantic hit is distinguishable from a literal one. A user who searches
/// "dog" and gets a photo with no dog in it deserves to see *why* it matched.
public struct SearchResult: Sendable, Equatable, Identifiable, Hashable {
    public var asset: LibraryAsset
    /// Which facet produced the hit.
    public var matchedScope: SearchScope
    /// Relevance, 0…1, when the backing index produces one.
    public var score: Double?

    public var id: AssetID { asset.id }

    public init(asset: LibraryAsset, matchedScope: SearchScope, score: Double? = nil) {
        self.asset = asset
        self.matchedScope = matchedScope
        self.score = score
    }
}

// MARK: - AIModelStatus

/// The state of one on-device model slot.
///
/// Model weights are **never in the repository** and are fetched or bundled
/// separately, so "not downloaded" is a normal steady state rather than an
/// error, and every AI surface must render usefully without it.
public struct AIModelStatus: Sendable, Equatable, Identifiable, Hashable {
    /// What the slot is for.
    public enum Purpose: ClosedWireEnum {
        case imageEmbedding
        case faceDetection
        case faceEmbedding
        case sceneTagging
        case unknown(String)

        public static let knownCases: [Purpose] = [
            .imageEmbedding, .faceDetection, .faceEmbedding, .sceneTagging,
        ]

        public init(rawValue: String) {
            self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
        }

        public var rawValue: String {
            switch self {
            case .imageEmbedding: "image_embedding"
            case .faceDetection: "face_detection"
            case .faceEmbedding: "face_embedding"
            case .sceneTagging: "scene_tagging"
            case let .unknown(raw): raw
            }
        }
    }

    /// Whether the model is usable right now.
    public enum Availability: Sendable, Equatable, Hashable {
        /// Weights are not present. The normal state before first use.
        case notDownloaded
        /// Weights are being fetched.
        case downloading(fractionComplete: Double)
        /// Ready to run.
        case ready
        /// The canonical model for this slot changed; existing output is stale
        /// and pending regeneration.
        case supersededBy(ModelSlot)
        /// This build cannot run the model on this hardware.
        case unsupportedOnThisDevice
    }

    public var slot: ModelSlot
    public var purpose: Purpose
    public var availability: Availability
    /// Assets still awaiting processing in this slot.
    public var pendingAssetCount: Int

    public var id: ModelSlot { slot }

    public init(
        slot: ModelSlot,
        purpose: Purpose,
        availability: Availability,
        pendingAssetCount: Int = 0
    ) {
        self.slot = slot
        self.purpose = purpose
        self.availability = availability
        self.pendingAssetCount = pendingAssetCount
    }

    /// Whether this slot can produce new output right now.
    public var isRunnable: Bool {
        availability == .ready
    }
}
