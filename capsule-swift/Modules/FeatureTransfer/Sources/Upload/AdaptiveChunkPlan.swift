import CapsuleDomain
import Foundation

// MARK: - AdaptiveChunkPlan

/// What chunk size the client is using, and why.
///
/// The split is normative (*Upload Protocol — Adaptive Chunk Sizing*): the
/// **server enforces bounds by rejection only** — 4 KiB alignment, the
/// `[4 KiB, 16 MiB]` range, the cumulative ceilings — and never adapts,
/// negotiates, or corrects; the **client owns adaptation**. The server's
/// `X-Capsule-Suggested-Chunk-Size` is a starting point, not an instruction.
///
/// This is modelled as a value type rather than a readout string so the disclosure
/// on ``UploadDetailView`` shows the *reasoning*, and so the ladder can be
/// asserted without a network.
public struct AdaptiveChunkPlan: Sendable, Equatable {
    // MARK: Protocol bounds

    /// Every chunk boundary is a multiple of this. Alignment is correct **by
    /// construction** here: every candidate is a doubling or halving of an
    /// aligned bound, so nothing depends on a runtime check.
    public static let alignmentBytes: UInt64 = 4 * 1024
    /// The floor for a non-final chunk.
    public static let minimumBytes: UInt64 = 256 * 1024
    /// The ceiling the server rejects past.
    public static let maximumBytes: UInt64 = 16 * 1024 * 1024

    // MARK: Adaptation constants

    /// Throughput is measured over a sliding window this long.
    public static let windowSeconds = 30
    /// No adjustment until this many chunks have gone at the current size…
    public static let warmUpChunks = 5
    /// …or this many bytes have. Decisions on a cold window oscillate.
    public static let warmUpBytes: UInt64 = 8 * 1024 * 1024
    /// Sustained throughput above this doubles the chunk size.
    public static let raiseAboveBytesPerSecond: Double = 5000000
    /// Sustained throughput below this halves it.
    public static let lowerBelowBytesPerSecond: Double = 1000000

    /// Why the current size is what it is.
    public enum Adjustment: Sendable, Equatable {
        /// Still inside the warm-up; the starting size stands.
        case warmingUp
        /// Sustained throughput cleared the raise threshold.
        case raised
        /// Sustained throughput fell below the lower threshold.
        case lowered
        /// Measured, and inside the band where nothing changes.
        case held
        /// An `adverse` link takes the conservative choice — the tier minimum —
        /// because adaptation must never regress effective throughput on a
        /// connection whose failures look like success to the OS.
        case conservativeForAdverseLink
        /// Nothing is measured yet.
        case unmeasured

        var titleKey: String {
            switch self {
            case .warmingUp: "ios.transfer.chunk.reason.warming_up"
            case .raised: "ios.transfer.chunk.reason.raised"
            case .lowered: "ios.transfer.chunk.reason.lowered"
            case .held: "ios.transfer.chunk.reason.held"
            case .conservativeForAdverseLink: "ios.transfer.chunk.reason.adverse"
            case .unmeasured: "ios.transfer.chunk.reason.unmeasured"
            }
        }
    }

    /// The server's starting suggestion for this blob's size tier.
    public var suggestedBytes: UInt64
    /// The size the client is actually sending at.
    public var currentBytes: UInt64
    public var adjustment: Adjustment

    public init(suggestedBytes: UInt64, currentBytes: UInt64, adjustment: Adjustment) {
        self.suggestedBytes = suggestedBytes
        self.currentBytes = currentBytes
        self.adjustment = adjustment
    }

    /// Whether the current size satisfies the protocol's alignment rule.
    /// Always true by construction; surfaced so a regression is visible rather
    /// than a `400` on the wire.
    public var isAligned: Bool {
        currentBytes.isMultiple(of: Self.alignmentBytes)
    }

    // MARK: Derivation

    /// The server's non-normative starting suggestion, by declared file size.
    ///
    /// `< 10 MB → 256 KiB`, `< 100 MB → 1 MiB`, `≥ 100 MB → 4 MiB`. Tunable
    /// server-side and explicitly *not* protocol surface, which is why it is a
    /// starting point the client is free to leave.
    public static func startingSize(declaredSize: UInt64) -> UInt64 {
        switch declaredSize {
        case ..<10000000: 256 * 1024
        case ..<100000000: 1024 * 1024
        default: 4 * 1024 * 1024
        }
    }

    /// Resolve the plan from what has actually been observed.
    ///
    /// - Parameters:
    ///   - declaredSize: the session's immutable declared size.
    ///   - observedBytesPerSecond: the sliding-window rate, `nil` if unmeasured.
    ///   - bytesSentAtCurrentSize: warm-up accumulator.
    ///   - chunksSentAtCurrentSize: warm-up accumulator.
    ///   - connection: the current class; `adverse` forces the conservative choice.
    public static func make(
        declaredSize: UInt64,
        observedBytesPerSecond: Double?,
        bytesSentAtCurrentSize: UInt64,
        chunksSentAtCurrentSize: Int,
        connection: ConnectionClass
    ) -> AdaptiveChunkPlan {
        let starting = startingSize(declaredSize: declaredSize)
        guard connection != .adverse else {
            return AdaptiveChunkPlan(
                suggestedBytes: starting,
                currentBytes: clamp(minimumBytes),
                adjustment: .conservativeForAdverseLink
            )
        }
        guard let rate = observedBytesPerSecond else {
            return AdaptiveChunkPlan(suggestedBytes: starting, currentBytes: starting, adjustment: .unmeasured)
        }
        let warmedUp = chunksSentAtCurrentSize >= warmUpChunks || bytesSentAtCurrentSize >= warmUpBytes
        guard warmedUp else {
            return AdaptiveChunkPlan(suggestedBytes: starting, currentBytes: starting, adjustment: .warmingUp)
        }
        if rate > raiseAboveBytesPerSecond {
            return AdaptiveChunkPlan(suggestedBytes: starting, currentBytes: clamp(starting * 2), adjustment: .raised)
        }
        if rate < lowerBelowBytesPerSecond {
            return AdaptiveChunkPlan(suggestedBytes: starting, currentBytes: clamp(starting / 2), adjustment: .lowered)
        }
        return AdaptiveChunkPlan(suggestedBytes: starting, currentBytes: clamp(starting), adjustment: .held)
    }

    /// Clamp a candidate into the protocol bounds. Candidates are doublings and
    /// halvings of aligned bounds, so the result stays 4 KiB-aligned.
    private static func clamp(_ candidate: UInt64) -> UInt64 {
        min(max(candidate, minimumBytes), maximumBytes)
    }
}
