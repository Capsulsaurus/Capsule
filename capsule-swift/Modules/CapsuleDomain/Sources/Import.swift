import CapsuleFoundation
import Foundation

// MARK: - SourceKind

/// The kind of local source an import came from — the closed enum inside a
/// scope's identity (*Asset Organization — Scope Grammar*).
public enum SourceKind: ClosedWireEnum {
    case cameraRoll
    case screenshots
    case appCollection
    case folder
    case watchedDirectory
    case removableVolume
    case unknown(String)

    public static let knownCases: [SourceKind] = [
        .cameraRoll, .screenshots, .appCollection, .folder, .watchedDirectory, .removableVolume,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .cameraRoll: "camera_roll"
        case .screenshots: "screenshots"
        case .appCollection: "app_collection"
        case .folder: "folder"
        case .watchedDirectory: "watched_dir"
        case .removableVolume: "removable_volume"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - ImportScope

/// The canonical identity of an import source (*Asset Organization — Scope
/// Grammar*).
///
/// The scope id is derived deterministically from `(platform, source_kind,
/// locator)`, so two devices of the same platform looking at the same source
/// compute the same scope and the mapping table needs no coordination protocol.
///
/// The per-platform locator is chosen for **stability across reinstall**, which
/// is why Android uses a relative path rather than a bucket id — a bucket id is
/// a hash of the display name and differs across devices and OS versions.
public struct ImportScope: Sendable, Equatable, Identifiable, Hashable {
    /// The deterministic scope id, computed in `capsule-core`. Never recomputed
    /// here: a Swift reimplementation would be a second, drift-prone source of
    /// a value two devices must agree on byte-for-byte.
    public var scopeID: String
    public var platform: PlatformTag
    public var sourceKind: SourceKind
    /// The canonical per-platform locator.
    public var locator: String

    public var id: String { scopeID }

    public init(scopeID: String, platform: PlatformTag, sourceKind: SourceKind, locator: String) {
        self.scopeID = scopeID
        self.platform = platform
        self.sourceKind = sourceKind
        self.locator = locator
    }
}

// MARK: - ImportMode

/// Whether the source files survive the import.
public enum ImportMode: ClosedWireEnum {
    /// The source is left in place.
    case copy
    /// The source is deleted **after** a durable storage verdict — never on the
    /// local library copy alone, so a crash mid-import cannot lose the only
    /// copy.
    case move
    case unknown(String)

    public static let knownCases: [ImportMode] = [.copy, .move]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .copy: "copy"
        case .move: "move"
        case let .unknown(raw): raw
        }
    }

    /// Whether this mode deletes the source, and therefore requires the
    /// verify-before-destroy gate.
    public var releasesSource: Bool {
        self == .move
    }
}

// MARK: - ImportCandidate

/// One file the scanner found, before any decision has been made about it.
public struct ImportCandidate: Sendable, Equatable, Identifiable, Hashable {
    /// A stable identity for the candidate within this scan.
    public var id: String
    /// The source locator — a path, a PhotoKit local identifier.
    public var locator: String
    /// The detected content type, or `nil` if the scanner could not tell.
    public var contentType: ContentType?
    /// Byte size, when the source reports it.
    public var byteSize: UInt64?
    /// Companion files that will be stacked with it — a RAW's JPEG, a Live
    /// Photo's clip, an XMP sidecar.
    public var companionLocators: [String]

    public init(
        id: String,
        locator: String,
        contentType: ContentType? = nil,
        byteSize: UInt64? = nil,
        companionLocators: [String] = []
    ) {
        self.id = id
        self.locator = locator
        self.contentType = contentType
        self.byteSize = byteSize
        self.companionLocators = companionLocators
    }
}

// MARK: - ImportScan

/// The result of scanning a source, before planning.
public struct ImportScan: Sendable, Equatable {
    public var scope: ImportScope
    public var candidates: [ImportCandidate]
    /// Locators the scanner could not read at all — a permissions problem, not
    /// a format problem. Surfaced rather than silently skipped.
    public var unreadableLocators: [String]

    public init(scope: ImportScope, candidates: [ImportCandidate], unreadableLocators: [String] = []) {
        self.scope = scope
        self.candidates = candidates
        self.unreadableLocators = unreadableLocators
    }

    /// Total bytes the scan found, where sizes are known.
    public var totalKnownBytes: UInt64 {
        candidates.compactMap(\.byteSize).reduce(0, +)
    }
}

// MARK: - ImportAction

/// What the planner decided to do with one candidate.
///
/// Every candidate gets an explicit decision — there is no "silently skipped"
/// bucket. A user who imports 400 photos and sees 380 arrive is owed the reason
/// for the other 20.
public enum ImportAction: Sendable, Equatable, Hashable {
    /// Import it as a new asset.
    case importAsset
    /// Already in the library under this content hash.
    case skipDuplicate(existingAssetID: String)
    /// The format has no importer in this build.
    case skipUnsupported(ContentType?)
    /// The file could not be read or decoded.
    case skipUnreadable
    /// Import it as a member of a stack alongside its companions.
    case importAsStackMember(stackType: StackType, role: StackRole)
}

// MARK: - ImportDecision

/// One candidate paired with the planner's decision about it.
///
/// A named struct rather than a tuple so the pair is `Equatable`, `Hashable`,
/// and readable at every call site — a plan review screen iterates these
/// directly.
public struct ImportDecision: Sendable, Equatable, Hashable, Identifiable {
    public var candidate: ImportCandidate
    public var action: ImportAction

    public var id: String { candidate.id }

    public init(candidate: ImportCandidate, action: ImportAction) {
        self.candidate = candidate
        self.action = action
    }

    /// Whether this decision actually produces an asset.
    public var isImporting: Bool {
        switch action {
        case .importAsset, .importAsStackMember: true
        case .skipDuplicate, .skipUnsupported, .skipUnreadable: false
        }
    }
}

// MARK: - ImportPlan

/// The plan a user confirms before anything is written.
///
/// **Plan and confirm are separate steps on purpose.** The destination, the
/// mode, and the count are all things a user must be able to see before a
/// bulk irreversible operation begins — particularly for
/// ``ImportMode/move``, which deletes their source files.
public struct ImportPlan: Sendable, Equatable, Identifiable {
    public var id: ImportID
    public var scope: ImportScope
    /// The resolved destination. **Always a container album, never a view** —
    /// resolution is explicit user pick → scope override → per-source-kind
    /// default → the owner's default pointer → the derived de facto album.
    public var destinationAlbumID: AlbumID
    /// Which resolution rule fired, recorded so a surprising destination is
    /// explainable after the fact.
    public var destinationRule: DestinationRule
    public var mode: ImportMode
    /// The upload policy this run will use.
    public var uploadPolicy: UploadPolicy
    /// Whether local bytes are released as the run proceeds. **Mutually
    /// exclusive with a staged upload policy** — streaming exists to release
    /// bytes quickly, staged defers exactly the upload release depends on — and
    /// the planner rejects the combination outright.
    public var isStreaming: Bool
    /// Per-candidate decisions.
    public var decisions: [ImportDecision]

    public init(
        id: ImportID,
        scope: ImportScope,
        destinationAlbumID: AlbumID,
        destinationRule: DestinationRule,
        mode: ImportMode,
        uploadPolicy: UploadPolicy,
        isStreaming: Bool,
        decisions: [ImportDecision]
    ) {
        self.id = id
        self.scope = scope
        self.destinationAlbumID = destinationAlbumID
        self.destinationRule = destinationRule
        self.mode = mode
        self.uploadPolicy = uploadPolicy
        self.isStreaming = isStreaming
        self.decisions = decisions
    }

    /// Which rule resolved the destination album.
    public enum DestinationRule: Sendable, Equatable, Hashable {
        case explicitUserPick
        case scopeOverride
        case sourceKindDefault
        case ownerDefaultPointer
        case derivedDefaultAlbum
    }

    /// How many candidates will actually be imported.
    public var importCount: Int {
        decisions.filter(\.isImporting).count
    }

    /// Whether the plan violates the staged × streaming exclusion. A plan for
    /// which this is `true` must be rejected at confirmation, never executed.
    public var violatesStagedStreamingExclusion: Bool {
        isStreaming && uploadPolicy == .staged
    }
}

// MARK: - ImportOutcome

/// What actually happened to one candidate.
///
/// ``imported(assetID:derivativesDeferred:)`` with deferred derivatives is a
/// **successful** import: the original is signed, encrypted, and verifiable, and
/// only the thumbnail is missing because this build has no codec for the format.
/// Counting it as a failure would make a HEIC-only library look like it lost
/// data.
public enum ImportOutcome: Sendable, Equatable, Hashable {
    case imported(assetID: String, derivativesDeferred: Bool)
    case duplicateSkipped(existingAssetID: String)
    case unsupported
    case unreadable
    case permissionDenied
    /// Some members of a stack imported and some did not.
    case partialStack(imported: [String], skipped: [String])
}

// MARK: - ImportProgressEvent

/// Progress emitted while an import runs.
///
/// A stream rather than a poll: an import is long, cancellable, and the UI must
/// show which file is being worked on right now. Every event carries enough
/// context to render a determinate progress view without the consumer keeping
/// its own tally.
public enum ImportProgressEvent: Sendable, Equatable {
    case started(importID: ImportID, totalCandidates: Int)
    case candidateStarted(index: Int, total: Int, locator: String)
    case candidateFinished(index: Int, locator: String, outcome: ImportOutcome)
    case finished(summary: ImportSummary)
    /// The run was cancelled. Everything already imported stays imported —
    /// cancellation is a stop, never a rollback.
    case cancelled(summary: ImportSummary)
}

// MARK: - ImportResult

/// One candidate's locator paired with what happened to it.
public struct ImportResult: Sendable, Equatable, Hashable, Identifiable {
    public var locator: String
    public var outcome: ImportOutcome

    public var id: String { locator }

    public init(locator: String, outcome: ImportOutcome) {
        self.locator = locator
        self.outcome = outcome
    }
}

// MARK: - ImportSummary

/// The tally of a finished or cancelled run.
public struct ImportSummary: Sendable, Equatable, Identifiable {
    public var id: ImportID
    public var results: [ImportResult]

    public init(id: ImportID, results: [ImportResult]) {
        self.id = id
        self.results = results
    }

    /// Successfully imported, deferred derivatives included.
    public var importedCount: Int {
        results.filter {
            if case .imported = $0.outcome { return true }
            return false
        }.count
    }

    /// Imported but without thumbnails, because this build has no codec. Also
    /// counted by ``importedCount`` — reported separately so a UI can say "N
    /// imported without previews" instead of implying a loss.
    public var deferredDerivativeCount: Int {
        results.filter {
            if case let .imported(_, deferred) = $0.outcome { return deferred }
            return false
        }.count
    }

    /// Candidates that did not import for any reason.
    public var skippedCount: Int {
        results.count - importedCount
    }
}
