import CapsuleFoundation
import Foundation

// MARK: - ImportScanProgress

/// How far a scan has got.
///
/// ``expectedTotal`` is optional because the two source families genuinely
/// differ: a PhotoKit fetch result knows its count before the first item is
/// read, while a directory walk or an archive stream does not know it until it
/// ends. Modelling that as an optional rather than a `-1` sentinel is what lets
/// the UI pick a determinate or an indeterminate indicator without guessing —
/// and stops it drawing a progress bar that would jump backwards.
///
/// Mirrors the Rust `ImportScanProgress` record.
public struct ImportScanProgress: Sendable, Equatable, Hashable {
    /// Candidates enumerated so far.
    public var itemsFound: Int
    /// Bytes accounted for so far, where the source reports sizes.
    public var bytesFound: UInt64
    /// The locator being read right now, for the "currently scanning" line.
    public var currentLocator: String?
    /// The total the source declared up front, or `nil` when it cannot.
    public var expectedTotal: Int?

    public init(
        itemsFound: Int,
        bytesFound: UInt64 = 0,
        currentLocator: String? = nil,
        expectedTotal: Int? = nil
    ) {
        self.itemsFound = itemsFound
        self.bytesFound = bytesFound
        self.currentLocator = currentLocator
        self.expectedTotal = expectedTotal
    }

    /// Whether a determinate indicator can honestly be drawn.
    public var isDeterminate: Bool {
        guard let expectedTotal else { return false }
        return expectedTotal > 0
    }

    /// Completed fraction, 0…1, or `nil` when the total is unknown.
    ///
    /// Clamped at 1 rather than allowed to exceed it: a source that under-reports
    /// its own count must not produce a bar that runs off the end.
    public var fraction: Double? {
        guard let expectedTotal, expectedTotal > 0 else { return nil }
        return min(1, Double(itemsFound) / Double(expectedTotal))
    }
}

// MARK: - ImportScanEvent

/// Progress emitted while a source is being enumerated.
///
/// A stream rather than one `async` call because a scan of a removable volume
/// or a Takeout archive is minutes long and must be abandonable. There is no
/// explicit cancel call: a scan writes nothing, so cancelling the task that
/// consumes the stream is a complete and safe stop, and a second cancellation
/// path would be a second thing to get wrong.
public enum ImportScanEvent: Sendable, Equatable {
    /// The scan opened. Carries the source's declared total when it has one.
    case started(expectedTotal: Int?)
    /// One progress tick.
    case progress(ImportScanProgress)
    /// The scan completed and produced its result.
    case finished(ImportScan)
    /// The stream was torn down before the source was exhausted. Nothing was
    /// written, so there is nothing to undo.
    case cancelled(itemsFound: Int)
}

// MARK: - ImportConflictKind

/// Why the planner could not decide a candidate's fate on its own.
///
/// Distinct from ``ImportAction``'s skip reasons, which are facts the planner
/// established (this is a duplicate; this format has no importer). A conflict is
/// a question only the user can answer, so it is carried separately and blocks
/// nothing else in the plan.
public enum ImportConflictKind: ClosedWireEnum {
    /// The same content hash is already in the library, but the incoming file
    /// carries metadata the existing asset does not.
    case duplicateWithNewMetadata
    /// A file of the same name in the same scope, with different content.
    case sameNameDifferentContent
    /// The library copy has edits that replacing it would discard.
    case existingIsEdited
    /// The candidate's scope resolves to a different album than the rest of the
    /// run, so importing it here would contradict a standing override.
    case destinationDiffers
    case unknown(String)

    public static let knownCases: [ImportConflictKind] = [
        .duplicateWithNewMetadata, .sameNameDifferentContent, .existingIsEdited, .destinationDiffers,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .duplicateWithNewMetadata: "duplicate_with_new_metadata"
        case .sameNameDifferentContent: "same_name_different_content"
        case .existingIsEdited: "existing_is_edited"
        case .destinationDiffers: "destination_differs"
        case let .unknown(raw): raw
        }
    }

    /// The resolutions this kind admits, in the order a picker should offer
    /// them.
    ///
    /// Not every resolution is meaningful for every conflict — merging two files
    /// that merely share a name into one stack would fabricate a relationship
    /// nobody asserted — and an option that cannot be honoured must not be
    /// offered. A kind this build does not recognise offers only the two
    /// non-destructive choices, because guessing what a newer writer meant is
    /// exactly how an unknown value turns into data loss.
    public var allowedResolutions: [ImportConflictResolution] {
        switch self {
        case .duplicateWithNewMetadata: [.skipIncoming, .mergeIntoExisting, .keepBoth]
        case .sameNameDifferentContent: [.keepBoth, .skipIncoming, .replaceExisting]
        case .existingIsEdited: [.keepBoth, .skipIncoming]
        case .destinationDiffers: [.keepBoth, .skipIncoming]
        case .unknown: [.keepBoth, .skipIncoming]
        }
    }

    /// The choice the planner pre-selects.
    ///
    /// Always the non-destructive one. A confirmation screen whose default
    /// discards data turns a mis-tap into a loss, and the whole reason plan and
    /// execute are separate calls is that a bulk irreversible operation must be
    /// consented to rather than defaulted into.
    public var defaultResolution: ImportConflictResolution {
        allowedResolutions.first ?? .skipIncoming
    }
}

// MARK: - ImportConflictResolution

/// What to do about one conflict.
public enum ImportConflictResolution: ClosedWireEnum {
    /// Import the incoming file alongside the existing asset.
    case keepBoth
    /// Do not import the incoming file. The existing asset is untouched.
    case skipIncoming
    /// Fold the incoming file's metadata into the existing asset without
    /// storing a second copy of the bytes.
    case mergeIntoExisting
    /// Import the incoming file and soft-delete the existing asset. The only
    /// destructive choice, and never a default.
    case replaceExisting
    case unknown(String)

    public static let knownCases: [ImportConflictResolution] = [
        .keepBoth, .skipIncoming, .mergeIntoExisting, .replaceExisting,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .keepBoth: "keep_both"
        case .skipIncoming: "skip_incoming"
        case .mergeIntoExisting: "merge_into_existing"
        case .replaceExisting: "replace_existing"
        case let .unknown(raw): raw
        }
    }

    /// Whether choosing this removes something the user already has.
    public var isDestructive: Bool {
        self == .replaceExisting
    }

    /// Whether choosing this still produces a new asset.
    public var admitsCandidate: Bool {
        switch self {
        case .keepBoth, .replaceExisting: true
        case .skipIncoming, .mergeIntoExisting, .unknown: false
        }
    }
}

// MARK: - ImportConflict

/// One candidate the planner will not decide without being told.
///
/// Mirrors the Rust `ImportConflict` record.
public struct ImportConflict: Sendable, Equatable, Hashable, Identifiable {
    /// The ``ImportCandidate/id`` this is about.
    public var candidateID: String
    /// The candidate's source locator, so the row can name the file.
    public var locator: String
    public var kind: ImportConflictKind
    /// The library asset the conflict is with, when there is one.
    public var existingAssetID: String?
    /// The resolution currently selected — the planner's default until a user
    /// changes it.
    public var resolution: ImportConflictResolution

    public var id: String { candidateID }

    public init(
        candidateID: String,
        locator: String,
        kind: ImportConflictKind,
        existingAssetID: String? = nil,
        resolution: ImportConflictResolution? = nil
    ) {
        self.candidateID = candidateID
        self.locator = locator
        self.kind = kind
        self.existingAssetID = existingAssetID
        self.resolution = resolution ?? kind.defaultResolution
    }

    /// Whether the selected resolution is one this kind actually admits.
    ///
    /// Checked rather than assumed: a resolution restored from a persisted plan
    /// written by a different build could name a choice this kind no longer
    /// offers, and executing it would honour a promise the UI never made.
    public var hasAdmissibleResolution: Bool {
        kind.allowedResolutions.contains(resolution)
    }
}

// MARK: - ImportItemStage

/// Where one item stands inside a running import.
///
/// The ladder is visible to the user because the stages fail differently and
/// take wildly different amounts of time: decoding is CPU-bound and fast,
/// encryption is fixed-cost, and upload is the one that stalls on a bad link.
/// Collapsing them into a single spinner would make a network stall look like a
/// hung app.
///
/// Mirrors the Rust `ImportItemStage` enum.
public enum ImportItemStage: ClosedWireEnum {
    /// Accepted into the run, not yet started.
    case queued
    /// Decoding, hashing, and extracting metadata.
    case processing
    /// Sealing the original and its derivatives.
    case encrypting
    /// Transferring to the home server.
    case uploading
    /// Finished successfully.
    case done
    /// Finished unsuccessfully. Retryable.
    case failed
    case unknown(String)

    public static let knownCases: [ImportItemStage] = [
        .queued, .processing, .encrypting, .uploading, .done, .failed,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .queued: "queued"
        case .processing: "processing"
        case .encrypting: "encrypting"
        case .uploading: "uploading"
        case .done: "done"
        case .failed: "failed"
        case let .unknown(raw): raw
        }
    }

    /// Whether the item is still moving.
    public var isActive: Bool {
        switch self {
        case .processing, .encrypting, .uploading: true
        case .queued, .done, .failed, .unknown: false
        }
    }

    /// Whether the item has reached a final state.
    public var isTerminal: Bool {
        self == .done || self == .failed
    }
}
