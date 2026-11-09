import CapsuleDomain
import Foundation

// MARK: - TransferBoard

/// The real-time transfer view's model: what every upload and fetch is doing,
/// folded from the transfer event stream (``TransferEventPort``) and nothing
/// else.
///
/// A pure fold, the same one the TUI's transfer view runs. Each
/// ``StampedTransferEvent`` carries the milliseconds since the view started
/// watching, and ``tick(to:)`` moves the clock forward between events; the
/// board never reads a clock, so a recorded stream replays into exactly the
/// frames it drew live.
///
/// What it derives:
///
/// - **Progress.** Uploads and ranged fetches both move a prefix of the blob:
///   the server's acknowledged offset, or the bytes a fetch has landed.
///   ``Transfer/held`` is the part the server held before this run sent
///   anything (a resume after a cut).
/// - **The chunk in flight**, and whether it is being retried or was refused
///   for a checksum mismatch. A chunk that needed either leaves a ``Scar``, so
///   the strip still shows where trouble was after the chunk lands.
/// - **Adaptive chunk size.** The length of each non-final upload chunk; a
///   change is the adaptive strategy resizing. A fetch's window is fixed.
/// - **Throughput** over the last ``throughputWindowMilliseconds`` of stream
///   time.
/// - **Custody.** Verdicts and receipts; a receipt marks an upload verified.
///
/// It stays bounded over a long sync whether or not transfers finish: finished
/// transfers past ``finishedKept`` go oldest first, an unfinished one silent
/// for ``staleAfterMilliseconds`` goes, at most ``transfersKept`` are held, and
/// scars and samples are capped. Age is the stream's own time, never a wall
/// clock.
public struct TransferBoard: Sendable, Equatable {
    // MARK: Bounds

    /// The span of stream time throughput is averaged over.
    public static let throughputWindowMilliseconds: UInt64 = 2000
    /// Finished transfers (verified, merged, rejected) kept for display.
    public static let finishedKept = 32
    /// Stream time an unfinished transfer may go without an event before it is
    /// dropped: nothing still moving is silent that long.
    public static let staleAfterMilliseconds: UInt64 = 300000
    /// Transfers held in all; the one silent longest goes past this.
    public static let transfersKept = 256
    /// Scars kept per transfer.
    public static let scarsKept = 64
    /// Distinct chunk sizes remembered per transfer, the current one last.
    public static let chunkSizesKept = 4

    // MARK: State

    /// The board's clock: the latest stamp or tick seen.
    public private(set) var nowMilliseconds: UInt64 = 0
    /// The transfers, oldest first.
    public private(set) var transfers: [Transfer] = []
    /// Events the view fell too far behind to receive.
    public private(set) var missed: UInt64 = 0
    /// Custody answers seen.
    public private(set) var custody = Custody()

    public init() {}

    /// Bytes per second across every transfer over the window ending now.
    public var throughput: UInt64 {
        transfers.reduce(0) { Self.saturatingAdd($0, $1.throughput(at: nowMilliseconds)) }
    }

    /// Transfers not yet finished.
    public var activeCount: Int {
        transfers.count(where: { !$0.phase.isFinished })
    }

    // MARK: Folding

    /// Fold one item of the feed in.
    public mutating func apply(_ item: TransferFeedItem) {
        switch item {
        case let .event(stamped): apply(stamped)
        case let .lagged(count): missed = Self.saturatingAdd(missed, count)
        }
    }

    /// Move the clock to `now` (never backwards), so throughput decays while
    /// nothing moves.
    public mutating func tick(to now: UInt64) {
        nowMilliseconds = max(nowMilliseconds, now)
        let clock = nowMilliseconds
        for index in transfers.indices {
            transfers[index].prune(at: clock)
        }
        dropStale()
    }

    /// Fold one event in.
    public mutating func apply(_ stamped: StampedTransferEvent) {
        tick(to: stamped.atMilliseconds)
        let now = nowMilliseconds
        switch stamped.event {
        case let .verify(_, durable, _, _):
            if durable { custody.durable += 1 } else { custody.notDurable += 1 }
        case let .receipt(_, blob, receiptSequence):
            receipt(for: blob, sequence: receiptSequence, at: now)
        case .rangeRequested, .rangeReceived, .fetchRetried, .fetchVerified, .integrityRejected:
            applyFetch(stamped.event, at: now)
        default:
            applyUpload(stamped.event, at: now)
        }
        dropOldFinished()
        capTotal()
    }

    private mutating func applyUpload(_ event: TransferEvent, at now: UInt64) {
        switch event {
        case let .sessionCreated(blob, session, size, resumeOffset):
            // A create after a lost session continues the same transfer; after
            // finalizing, it is a new upload of the same blob.
            let index = entry(.upload, blob, at: now) { $0 == .active }
            transfers[index].open(session: session, size: size)
            transfers[index].resume(from: resumeOffset)
        case let .sessionResumed(blob, session, size, offset):
            let index = live(.upload, blob, at: now)
            transfers[index].open(session: session, size: size)
            transfers[index].resume(from: offset)
        case let .chunkSent(blob, _, offset, length):
            transfers[live(.upload, blob, at: now)].sent(offset: offset, length: length)
        case let .chunkAcked(blob, _, offset, _, ackedOffset):
            transfers[live(.upload, blob, at: now)].acked(from: offset, to: ackedOffset, at: now)
        case let .chunkRetried(blob, _, offset, length, reason):
            transfers[live(.upload, blob, at: now)].retried(offset: offset, length: length, reason: reason)
        case let .checksumRejected(blob, _, offset, length):
            transfers[live(.upload, blob, at: now)].rejected(offset: offset, length: length)
        case let .finalize(blob, session, size):
            transfers[live(.upload, blob, at: now)].finalizing(session: session, size: size)
        case let .merged(blob, _):
            let index = live(.upload, blob, at: now)
            transfers[index].active = nil
            transfers[index].phase = .merged
        default:
            break
        }
    }

    private mutating func applyFetch(_ event: TransferEvent, at now: UInt64) {
        switch event {
        case let .rangeRequested(blob, offset, maxLength):
            let index = live(.fetch, blob, at: now)
            transfers[index].active = ActiveChunk(offset: offset, length: maxLength, state: .inFlight)
            // A fetch keeps one window for its whole run (only the tail asks
            // for less), so its first bounded window is its chunk size.
            if let maxLength, transfers[index].chunkSizes.isEmpty {
                transfers[index].chunkSizes.append(maxLength)
            }
        case let .rangeReceived(blob, offset, length):
            let index = live(.fetch, blob, at: now)
            transfers[index].moved(length, at: now)
            transfers[index].done = Self.saturatingAdd(offset, length)
            transfers[index].active = nil
        case let .fetchRetried(blob, offset, stalled, waitMilliseconds):
            let index = live(.fetch, blob, at: now)
            let length = transfers[index].active?.length
            transfers[index].retries += 1
            transfers[index].lastRetry = .fetch(stalled: stalled, waitMilliseconds: waitMilliseconds)
            transfers[index].scar(offset: offset, length: length, kind: .retried)
            transfers[index].active = ActiveChunk(offset: offset, length: length, state: .retrying)
            transfers[index].realign(to: offset)
        case let .fetchVerified(blob, size):
            let index = live(.fetch, blob, at: now)
            transfers[index].size = size
            transfers[index].done = size
            transfers[index].active = nil
            transfers[index].phase = .verified
        case let .integrityRejected(blob, size):
            let index = live(.fetch, blob, at: now)
            transfers[index].size = size
            transfers[index].rejects += 1
            transfers[index].active = nil
            transfers[index].phase = .integrityRejected
        default:
            break
        }
    }

    /// A receipt verifies the latest upload of the blob, whatever its phase;
    /// one for a blob this view never saw uploaded is counted and nothing more.
    private mutating func receipt(for blob: BlobHash, sequence: UInt64, at now: UInt64) {
        custody.receipts += 1
        guard let index = transfers.lastIndex(where: { $0.direction == .upload && $0.blob == blob }) else {
            return
        }
        transfers[index].lastMilliseconds = now
        transfers[index].receiptSequence = sequence
        guard transfers[index].phase != .merged else { return }
        transfers[index].done = transfers[index].size ?? transfers[index].done
        transfers[index].active = nil
        transfers[index].phase = .verified
    }

    // MARK: Entries

    /// The live transfer of `blob` that way, or a new one (the stream may have
    /// lagged past its start).
    private mutating func live(_ direction: Direction, _ blob: BlobHash, at now: UInt64) -> Int {
        entry(direction, blob, at: now) { !$0.isFinished }
    }

    /// The latest transfer of `blob` that way whose phase `continues`, or a
    /// new one, with its last-event time set to `now`.
    private mutating func entry(
        _ direction: Direction,
        _ blob: BlobHash,
        at now: UInt64,
        continues: (Phase) -> Bool
    ) -> Int {
        let found = transfers.lastIndex { $0.direction == direction && $0.blob == blob && continues($0.phase) }
        let index: Int
        if let found {
            index = found
        } else {
            transfers.append(Transfer(blob: blob, direction: direction, atMilliseconds: now))
            index = transfers.count - 1
        }
        transfers[index].lastMilliseconds = now
        return index
    }

    // MARK: Bounds

    /// Drop unfinished transfers silent for ``staleAfterMilliseconds``.
    private mutating func dropStale() {
        let now = nowMilliseconds
        transfers.removeAll { !$0.phase.isFinished && now - min(now, $0.lastMilliseconds) >= Self.staleAfterMilliseconds }
    }

    /// Hold at most ``transfersKept``: drop the one silent longest until it
    /// fits.
    private mutating func capTotal() {
        while transfers.count > Self.transfersKept {
            guard let oldest = transfers.indices.min(by: { transfers[$0].lastMilliseconds < transfers[$1].lastMilliseconds })
            else { return }
            transfers.remove(at: oldest)
        }
    }

    /// Keep the newest ``finishedKept`` finished transfers.
    private mutating func dropOldFinished() {
        var excess = transfers.count(where: { $0.phase.isFinished }) - Self.finishedKept
        guard excess > 0 else { return }
        transfers.removeAll { transfer in
            guard excess > 0, transfer.phase.isFinished else { return false }
            excess -= 1
            return true
        }
    }

    // MARK: Arithmetic

    /// Whether a sample at `milliseconds` is inside the throughput window
    /// ending at `now`.
    static func inWindow(_ milliseconds: UInt64, now: UInt64) -> Bool {
        guard now >= throughputWindowMilliseconds else { return true }
        return milliseconds > now - throughputWindowMilliseconds
    }

    static func saturatingAdd(_ lhs: UInt64, _ rhs: UInt64) -> UInt64 {
        let (sum, overflow) = lhs.addingReportingOverflow(rhs)
        return overflow ? .max : sum
    }
}
