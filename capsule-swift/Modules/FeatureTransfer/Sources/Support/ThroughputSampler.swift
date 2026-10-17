import CapsuleDomain
import Foundation

// MARK: - ThroughputSampler

/// Observed transfer rate, derived from the offsets the server reports.
///
/// ``UploadSession`` carries no rate: the authoritative offset is the only
/// progress fact the protocol defines (*Upload Protocol — Idempotency and
/// Resumption*), and inventing a server-side rate field would add wire surface
/// for a display concern. So the client derives it, the same way it derives the
/// adaptive chunk size: by measuring what actually moved.
///
/// Exponentially smoothed rather than instantaneous, because a chunked upload's
/// raw offset deltas are a staircase — an unsmoothed readout would flick
/// between zero and a spike on every chunk boundary and read as broken.
public struct ThroughputSampler: Sendable, Equatable {
    /// How much weight a new sample carries. 0.3 settles within a few chunks
    /// without chasing every staircase step.
    public static let smoothing = 0.3

    /// A sample older than this is treated as a fresh start rather than
    /// averaged in: the transfer was paused, backgrounded, or resumed, and
    /// spreading the gap across the average would report a rate that never
    /// happened.
    public static let staleAfterSeconds: Int64 = 30

    private var lastOffset: UInt64?
    private var lastSampledAt: CapsuleTimestamp?
    private var smoothed: Double?

    public init() {}

    /// The current smoothed rate in bytes per second, or `nil` before two
    /// usable samples exist. `nil` is rendered as "measuring", never as zero —
    /// a stalled transfer and an unmeasured one are different facts.
    public var bytesPerSecond: Double? { smoothed }

    /// Fold in the offset the server reported at `instant`.
    ///
    /// A backwards offset — which the protocol permits after a re-align onto an
    /// authoritative `HEAD` — resets the sampler instead of producing a
    /// negative rate.
    public mutating func record(offset: UInt64, at instant: CapsuleTimestamp) {
        defer {
            lastOffset = offset
            lastSampledAt = instant
        }
        guard let previousOffset = lastOffset, let previousInstant = lastSampledAt else { return }
        let elapsed = instant.epochSeconds - previousInstant.epochSeconds
        guard elapsed > 0, elapsed <= Self.staleAfterSeconds else {
            smoothed = nil
            return
        }
        guard offset >= previousOffset else {
            smoothed = nil
            return
        }
        let rate = Double(offset - previousOffset) / Double(elapsed)
        smoothed = smoothed.map { $0 + Self.smoothing * (rate - $0) } ?? rate
    }
}

// MARK: - ThroughputBook

/// One sampler per session, keyed by upload id.
///
/// A dictionary rather than a field on the row model because rows are rebuilt
/// from scratch on every `changes()` emission — the measurement has to outlive
/// the projection it feeds.
public struct ThroughputBook: Sendable {
    private var samplers: [UploadID: ThroughputSampler] = [:]

    public init() {}

    /// Fold in a whole snapshot of sessions, and forget any session that is no
    /// longer present so a cancelled upload cannot leak its sampler.
    public mutating func record(_ sessions: [UploadSession], at instant: CapsuleTimestamp) {
        var updated: [UploadID: ThroughputSampler] = [:]
        updated.reserveCapacity(sessions.count)
        for session in sessions {
            var sampler = samplers[session.id] ?? ThroughputSampler()
            sampler.record(offset: session.offset, at: instant)
            updated[session.id] = sampler
        }
        samplers = updated
    }

    /// The smoothed rate for one session, if it has been measured.
    public func rate(for id: UploadID) -> Double? {
        samplers[id]?.bytesPerSecond
    }

    /// The aggregate rate across every measured session — what the header ring
    /// reports.
    public var aggregateBytesPerSecond: Double? {
        let rates = samplers.values.compactMap(\.bytesPerSecond)
        return rates.isEmpty ? nil : rates.reduce(0, +)
    }
}
