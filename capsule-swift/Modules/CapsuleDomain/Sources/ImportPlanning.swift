import CapsuleFoundation
import Foundation

// MARK: - Plan arithmetic

/// The figures the confirmation screen is built from.
///
/// Kept beside ``ImportSpaceOutlook`` rather than on ``ImportPlan`` itself
/// because they exist for one screen: a plan's *identity* is its decisions and
/// its destination, and the counts, the byte total, and the conflict gate are
/// all questions asked once, at the point of consent.
public extension ImportPlan {
    /// How many candidates the planner decided not to import.
    var skipCount: Int {
        decisions.count - importCount
    }

    /// Bytes the run will write, counting only what it will actually import.
    ///
    /// Candidates whose size the source did not report contribute nothing,
    /// which makes this a **lower bound** — the space meter is therefore
    /// optimistic in exactly the direction that keeps it from crying wolf on a
    /// source that under-reports.
    var estimatedByteSize: UInt64 {
        decisions.filter(\.isImporting).compactMap(\.candidate.byteSize).reduce(0, +)
    }

    /// Whether every conflict carries a resolution its kind admits.
    ///
    /// The confirm action is gated on this rather than on "no conflicts": a
    /// plan with four answered conflicts is a plan the user has fully consented
    /// to, and refusing it would make conflicts a dead end instead of a
    /// decision.
    var conflictsAreResolved: Bool {
        conflicts.allSatisfy { $0.hasAdmissibleResolution }
    }

    /// The plan with one conflict answered.
    ///
    /// Returns a new value rather than mutating in place so a view model can
    /// keep the planner's original alongside the user's edits and show what
    /// changed.
    func resolving(
        _ candidateID: String,
        as resolution: ImportConflictResolution
    ) -> ImportPlan {
        var copy = self
        guard let index = copy.conflicts.firstIndex(where: { $0.candidateID == candidateID }) else {
            return copy
        }
        copy.conflicts[index].resolution = resolution
        return copy
    }
}

// MARK: - Summary arithmetic

public extension ImportSummary {
    /// The results a retry could still turn into imports.
    ///
    /// Only ``ImportOutcome/failed(_:)`` qualifies. An unsupported format and a
    /// duplicate are settled facts, and offering to retry them would promise an
    /// outcome the retry cannot produce.
    var failedResults: [ImportResult] {
        results.filter {
            if case .failed = $0.outcome { return true }
            return false
        }
    }
}

// MARK: - ImportSpaceOutlook

/// Whether the device has room for a plan, and what to do if it barely does.
///
/// Three verdicts rather than a boolean because the middle one has a remedy
/// that is not "delete something": a streaming import releases each original as
/// soon as the server proves custody, so a run that would not fit all at once
/// fits comfortably one item at a time. Reporting only "fits" or "does not fit"
/// would send a user to free space they did not need to free.
///
/// Derived on the client from the plan and the volume's own figures rather than
/// carried on ``ImportPlan``: free space changes while the confirmation screen
/// is open, and a number baked into the plan would be stale by the time it is
/// read.
public struct ImportSpaceOutlook: Sendable, Equatable, Hashable {
    /// The three verdicts, in increasing severity.
    public enum State: Sendable, Equatable, Hashable {
        /// The plan fits with room to spare.
        case comfortable
        /// The plan fits, but would leave the volume tight. Streaming is the
        /// remedy.
        case streamingRecommended
        /// The plan does not fit even with streaming; bytes must be freed
        /// first.
        case insufficient
    }

    /// Bytes the plan will write.
    public var requiredBytes: UInt64
    /// Free space on the volume, or `nil` when the platform will not say.
    public var availableBytes: UInt64?
    /// Headroom deliberately left unspent, so an import cannot drive the device
    /// to zero free bytes and take the rest of the system down with it.
    public var reserveBytes: UInt64
    public var state: State
    /// How many bytes must be freed before the plan can run at all. Zero unless
    /// ``state`` is ``State/insufficient``.
    public var shortfallBytes: UInt64

    /// The default reserve: 2 GiB, roughly what iOS wants free before it starts
    /// evicting other apps' caches.
    public static let defaultReserveBytes: UInt64 = 2 * 1073741824

    /// Assess a plan against a volume.
    ///
    /// An unknown free-space figure yields ``State/comfortable``: the honest
    /// answer to "we cannot measure the disk" is not to block the import, and a
    /// warning drawn from a number nobody has is a warning users learn to
    /// ignore.
    public static func assess(
        requiredBytes: UInt64,
        availableBytes: UInt64?,
        reserveBytes: UInt64 = ImportSpaceOutlook.defaultReserveBytes
    ) -> ImportSpaceOutlook {
        guard let availableBytes else {
            return ImportSpaceOutlook(
                requiredBytes: requiredBytes,
                availableBytes: nil,
                reserveBytes: reserveBytes,
                state: .comfortable,
                shortfallBytes: 0
            )
        }
        let usable = availableBytes > reserveBytes ? availableBytes - reserveBytes : 0
        if requiredBytes > usable {
            return ImportSpaceOutlook(
                requiredBytes: requiredBytes,
                availableBytes: availableBytes,
                reserveBytes: reserveBytes,
                state: .insufficient,
                shortfallBytes: requiredBytes - usable
            )
        }
        // Half the usable headroom is the line: past it the run leaves the
        // volume with less slack than it consumed, which is where a
        // release-as-you-go run stops being an optimisation and starts being
        // the only comfortable way to do it.
        let state: State = requiredBytes * 2 > usable ? .streamingRecommended : .comfortable
        return ImportSpaceOutlook(
            requiredBytes: requiredBytes,
            availableBytes: availableBytes,
            reserveBytes: reserveBytes,
            state: state,
            shortfallBytes: 0
        )
    }

    /// The share of free space the plan would consume, 0…1, for the meter.
    /// Zero when free space is unknown.
    public var fractionOfAvailable: Double {
        guard let availableBytes, availableBytes > 0 else { return 0 }
        return min(1, Double(requiredBytes) / Double(availableBytes))
    }

    /// Whether the plan may be confirmed at all.
    public var permitsImport: Bool {
        state != .insufficient
    }
}

// MARK: - ImportSessionRecord

/// One past or running import, as the history list shows it.
///
/// Keeps the destination *and the rule that chose it* rather than the album
/// alone, for the same reason ``ImportPlan`` does: "why did those photos land
/// there" is a question asked days later, and an answer that requires
/// re-deriving the resolution ladder against today's settings would be a
/// different answer from the one that actually fired.
///
/// Mirrors the Rust `ImportSession` record.
public struct ImportSessionRecord: Sendable, Equatable, Identifiable {
    public var id: ImportID
    public var scope: ImportScope
    public var destinationAlbumID: AlbumID
    public var destinationRule: ImportPlan.DestinationRule
    public var mode: ImportMode
    public var startedAt: CapsuleTimestamp
    /// `nil` while the run is still going.
    public var finishedAt: CapsuleTimestamp?
    public var summary: ImportSummary
    /// Whether the run stopped early. Everything already imported stayed
    /// imported — cancellation is a stop, never a rollback.
    public var wasCancelled: Bool

    public init(
        id: ImportID,
        scope: ImportScope,
        destinationAlbumID: AlbumID,
        destinationRule: ImportPlan.DestinationRule,
        mode: ImportMode,
        startedAt: CapsuleTimestamp,
        finishedAt: CapsuleTimestamp? = nil,
        summary: ImportSummary,
        wasCancelled: Bool = false
    ) {
        self.id = id
        self.scope = scope
        self.destinationAlbumID = destinationAlbumID
        self.destinationRule = destinationRule
        self.mode = mode
        self.startedAt = startedAt
        self.finishedAt = finishedAt
        self.summary = summary
        self.wasCancelled = wasCancelled
    }

    /// How the run ended, as one closed choice a row can render.
    public enum Outcome: Sendable, Equatable, Hashable {
        case running
        case completed
        case completedWithFailures(count: Int)
        case cancelled
    }

    /// The run's standing.
    ///
    /// A run with failures is reported as such rather than as a plain success:
    /// the failures are retryable and the user is the only one who can decide
    /// to retry them, so hiding the count would hide the decision.
    public var outcome: Outcome {
        guard finishedAt != nil else { return .running }
        if wasCancelled { return .cancelled }
        let failures = summary.failedResults.count
        return failures == 0 ? .completed : .completedWithFailures(count: failures)
    }

    /// Locators that can be retried.
    public var retryableLocators: [String] {
        summary.failedResults.map(\.locator)
    }
}
