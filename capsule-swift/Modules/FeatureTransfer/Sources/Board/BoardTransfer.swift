import CapsuleDomain
import Foundation

// MARK: - TransferBoard vocabulary

public extension TransferBoard {
    /// Which way the bytes move.
    enum Direction: Sendable, Equatable, Hashable {
        /// To the server: an upload session.
        case upload
        /// From the server: a ranged fetch.
        case fetch
    }

    /// What the chunk in flight is doing.
    enum ChunkState: Sendable, Equatable, Hashable {
        /// Sent (or requested) and not yet answered.
        case inFlight
        /// Refused or lost; it is sent again after a backoff.
        case retrying
        /// The server refused it for a checksum mismatch; it is sent again.
        case rejected

        /// How the bytes under a chunk in this state are drawn.
        var cell: Cell {
            switch self {
            case .inFlight: .inFlight
            case .retrying: .retrying
            case .rejected: .rejected
            }
        }
    }

    /// The one chunk a transfer has in flight: the SDK sends sequentially.
    struct ActiveChunk: Sendable, Equatable, Hashable {
        /// The chunk's first byte.
        public var offset: UInt64
        /// Its length, or `nil` for an open-ended fetch of the remainder.
        public var length: UInt64?
        /// What it is doing.
        public var state: ChunkState
    }

    /// What went wrong with a chunk that has since moved on.
    enum ScarKind: Sendable, Equatable, Hashable {
        /// It needed a retry.
        case retried
        /// It was refused for a checksum mismatch.
        case rejected
    }

    /// Where trouble was, after the chunk has moved on: a byte range and what
    /// happened there.
    struct Scar: Sendable, Equatable, Hashable {
        /// The range's first byte.
        public var offset: UInt64
        /// Its length, or `nil` to the end of the blob.
        public var length: UInt64?
        /// What happened.
        public var kind: ScarKind
    }

    /// Why the last retry happened, as the stream said.
    enum RetryNote: Sendable, Equatable, Hashable {
        /// An upload chunk was retried.
        case upload(TransferRetryReason)
        /// A ranged fetch brought nothing and backs off.
        case fetch(stalled: Bool, waitMilliseconds: UInt64)
    }

    /// Where a transfer is in its life.
    enum Phase: Sendable, Equatable, Hashable {
        /// Moving bytes, or about to.
        case active
        /// Every byte of an upload reached the server; it is finalizing.
        case finalizing
        /// A custody receipt verified the upload, or the fetched bytes hashed
        /// to the address.
        case verified
        /// The server already held the blob; nothing was sent.
        case merged
        /// The fetched bytes did not hash to the requested address and were
        /// discarded.
        case integrityRejected

        /// No further event changes this transfer; a new one for the same blob
        /// starts afresh.
        public var isFinished: Bool {
            switch self {
            case .verified, .merged, .integrityRejected: true
            case .active, .finalizing: false
            }
        }
    }

    /// One cell of a transfer's chunk strip: what the bytes it covers are
    /// doing.
    enum Cell: Sendable, Equatable, Hashable {
        /// Not sent yet.
        case pending
        /// Held by the server before this run sent anything.
        case held
        /// Acknowledged (or landed, for a fetch).
        case acked
        /// Acknowledged after a retry.
        case ackedAfterRetry
        /// Acknowledged after a checksum refusal.
        case ackedAfterReject
        /// Verified: a custody receipt, or a matching hash.
        case verified
        /// In flight now.
        case inFlight
        /// Being retried now.
        case retrying
        /// Refused for a checksum mismatch, or the whole fetch failed its hash.
        case rejected
    }

    /// Custody answers seen, across every asset.
    struct Custody: Sendable, Equatable, Hashable {
        /// Assets the server called durable.
        public var durable: UInt64 = 0
        /// Assets with at least one blob not durable.
        public var notDurable: UInt64 = 0
        /// Receipts verified.
        public var receipts: UInt64 = 0
    }
}

// MARK: - TransferBoard.Transfer

public extension TransferBoard {
    /// Everything the view shows about one transfer.
    struct Transfer: Sendable, Equatable, Hashable {
        /// The blob's content address.
        public let blob: BlobHash
        /// Which way the bytes move.
        public let direction: Direction
        /// The current upload session, once one is open.
        public internal(set) var session: UploadID?
        /// The blob's size, once the stream has said it.
        public internal(set) var size: UInt64?
        /// The authoritative prefix: bytes acknowledged (upload) or landed
        /// (fetch).
        public internal(set) var done: UInt64 = 0
        /// The prefix the server held before this run sent anything.
        public internal(set) var held: UInt64 = 0
        /// The chunk in flight.
        public internal(set) var active: ActiveChunk?
        /// Where chunks needed a retry or were refused, newest last.
        public internal(set) var scars: [Scar] = []
        /// Distinct upload sessions it ran in (more than one after a lost
        /// session).
        public internal(set) var sessions = 0
        /// Times the transfer resumed from bytes the server already held.
        public internal(set) var resumes = 0
        /// The offset of the last resume.
        public internal(set) var resumedAt: UInt64?
        /// Retries taken.
        public internal(set) var retries = 0
        /// Checksum refusals (upload) or failed hashes (fetch).
        public internal(set) var rejects = 0
        /// Why the last retry happened.
        public internal(set) var lastRetry: RetryNote?
        /// Distinct chunk sizes, the current one last.
        public internal(set) var chunkSizes: [UInt64] = []
        /// How many times the adaptive strategy resized the chunk.
        public internal(set) var chunkSizeChanges = 0
        /// Bytes moved by this run.
        public internal(set) var moved: UInt64 = 0
        /// When the first event for it arrived.
        public let startedMilliseconds: UInt64
        /// When the last event for it arrived.
        public internal(set) var lastMilliseconds: UInt64
        /// The receipt that verified an upload.
        public internal(set) var receiptSequence: UInt64?
        /// Where it is in its life.
        public internal(set) var phase: Phase = .active
        /// Bytes moved, within the throughput window.
        var samples: [Sample] = []

        init(blob: BlobHash, direction: Direction, atMilliseconds: UInt64) {
            self.blob = blob
            self.direction = direction
            startedMilliseconds = atMilliseconds
            lastMilliseconds = atMilliseconds
        }

        /// The current chunk size, once a non-final chunk has been sent.
        public var chunkSize: UInt64? { chunkSizes.last }

        /// Bytes per second over the ``TransferBoard/throughputWindowMilliseconds``
        /// ending at `now`: the window is `(now - window, now]`, or everything
        /// before the clock has run a whole window.
        public func throughput(at now: UInt64) -> UInt64 {
            let bytes = samples
                .filter { TransferBoard.inWindow($0.milliseconds, now: now) }
                .reduce(UInt64(0)) { TransferBoard.saturatingAdd($0, $1.bytes) }
            let (perSecond, overflow) = bytes.multipliedReportingOverflow(by: 1000)
            return (overflow ? UInt64.max : perSecond) / TransferBoard.throughputWindowMilliseconds
        }

        /// The chunk strip in `wanted` cells (fewer when the blob has fewer
        /// bytes), or `nil` while the size is unknown or zero.
        public func cells(_ wanted: Int) -> [Cell]? {
            guard let size, size > 0, wanted > 0 else { return nil }
            let cells = min(UInt64(wanted), size)
            let bound = { (index: UInt64) -> UInt64 in
                let (high, low) = size.multipliedFullWidth(by: index)
                return cells.dividingFullWidth((high, low)).quotient
            }
            return (0 ..< cells).map { cell(from: bound($0), to: bound($0 + 1), size: size) }
        }

        /// The state of the bytes `low ..< high` of a blob of `size` bytes.
        private func cell(from low: UInt64, to high: UInt64, size: UInt64) -> Cell {
            switch phase {
            case .verified: return .verified
            case .integrityRejected: return .rejected
            case .active, .finalizing, .merged: break
            }
            let overlaps = { (offset: UInt64, length: UInt64?) -> Bool in
                let end = length.map { TransferBoard.saturatingAdd(offset, $0) } ?? size
                return offset < high && low < end
            }
            if let active, overlaps(active.offset, active.length) { return active.state.cell }
            if high <= held { return .held }
            if high > done { return .pending }
            var cell = Cell.acked
            for scar in scars where overlaps(scar.offset, scar.length) {
                switch scar.kind {
                case .rejected: return .ackedAfterReject
                case .retried: cell = .ackedAfterRetry
                }
            }
            return cell
        }

        // MARK: Folding

        /// The server's offset is `offset`: everything below it is held,
        /// nothing above it is.
        mutating func realign(to offset: UInt64) {
            done = offset
            held = min(held, offset)
        }

        /// The transfer now runs in `newSession` over a blob of `newSize`
        /// bytes; a session not seen last counts as one more.
        mutating func open(session newSession: UploadID, size newSize: UInt64) {
            if session != newSession { sessions += 1 }
            session = newSession
            size = newSize
            active = nil
        }

        /// A resume reported `offset` as already held. Before this run moved a
        /// byte, that prefix is the server's from an earlier run.
        mutating func resume(from offset: UInt64) {
            if offset > 0 {
                resumes += 1
                resumedAt = offset
            }
            if moved == 0 { held = offset }
            realign(to: offset)
        }

        mutating func scar(offset: UInt64, length: UInt64?, kind: ScarKind) {
            if scars.count == TransferBoard.scarsKept { scars.removeFirst() }
            scars.append(Scar(offset: offset, length: length, kind: kind))
        }

        mutating func sent(offset: UInt64, length: UInt64) {
            active = ActiveChunk(offset: offset, length: length, state: .inFlight)
            // The last chunk is whatever remains, not the adaptive size.
            let tail = size.map { TransferBoard.saturatingAdd(offset, length) >= $0 } ?? false
            if tail || chunkSize == length { return }
            if !chunkSizes.isEmpty { chunkSizeChanges += 1 }
            chunkSizes.removeAll { $0 == length }
            if chunkSizes.count == TransferBoard.chunkSizesKept { chunkSizes.removeFirst() }
            chunkSizes.append(length)
        }

        /// The server acknowledged the chunk at `offset`; it now holds
        /// `ackedOffset` bytes, which is less than the chunk's end after a
        /// short write.
        mutating func acked(from offset: UInt64, to ackedOffset: UInt64, at now: UInt64) {
            moved(ackedOffset > offset ? ackedOffset - offset : 0, at: now)
            done = ackedOffset
            active = nil
        }

        /// An upload chunk will be sent again, or the transfer moves on to
        /// the offset the server named.
        mutating func retried(offset: UInt64, length: UInt64, reason: TransferRetryReason) {
            retries += 1
            lastRetry = .upload(reason)
            scar(offset: offset, length: length, kind: .retried)
            active = ActiveChunk(offset: offset, length: length, state: .retrying)
            if case let .offsetRealign(target) = reason { realign(to: target) }
        }

        /// The server refused a chunk for a checksum mismatch.
        mutating func rejected(offset: UInt64, length: UInt64) {
            rejects += 1
            scar(offset: offset, length: length, kind: .rejected)
            active = ActiveChunk(offset: offset, length: length, state: .rejected)
        }

        /// Every byte reached the server, which is finalizing the blob.
        mutating func finalizing(session finalSession: UploadID, size finalSize: UInt64) {
            session = finalSession
            size = finalSize
            done = finalSize
            active = nil
            phase = .finalizing
        }

        mutating func moved(_ bytes: UInt64, at milliseconds: UInt64) {
            moved = TransferBoard.saturatingAdd(moved, bytes)
            samples.append(Sample(milliseconds: milliseconds, bytes: bytes))
        }

        mutating func prune(at now: UInt64) {
            samples.removeAll { !TransferBoard.inWindow($0.milliseconds, now: now) }
        }
    }

    /// Bytes moved at one instant, kept while inside the throughput window.
    struct Sample: Sendable, Equatable, Hashable {
        /// When they moved, on the stream's clock.
        var milliseconds: UInt64
        /// How many.
        var bytes: UInt64
    }
}
