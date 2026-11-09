import CapsuleDomain
import CapsuleFoundation
import Testing

@testable import FeatureTransfer

/// The board's fold over a ranged fetch, the chunk strip's geometry, and the
/// bounds that keep the board small over a long sync whether or not its
/// transfers ever finish.
@Suite("Transfer board: fetches and bounds")
struct TransferBoardFetchAndBoundsTests {
    private typealias Fixture = BoardFixture
    private let kib = BoardFixture.kib
    private let fetched = BlobHash("f")

    private func only(_ board: TransferBoard) throws -> TransferBoard.Transfer {
        try #require(board.transfers.count == 1)
        return board.transfers[0]
    }

    private func createdBlob(_ name: String) -> TransferEvent {
        Fixture.created(kib, session: UploadID("s"), blob: BlobHash(name))
    }

    @Test("A fetch tracks its landed prefix, its retries, and its verdict")
    func fetchLifecycle() throws {
        var board = Fixture.board([
            Fixture.at(0, .rangeRequested(blob: fetched, offset: 0, maxLength: 512)),
            Fixture.at(0, .rangeReceived(blob: fetched, offset: 0, length: 512)),
            Fixture.at(0, .rangeRequested(blob: fetched, offset: 512, maxLength: 512)),
            Fixture.at(0, .fetchRetried(blob: fetched, offset: 512, stalled: true, waitMilliseconds: 40)),
        ])
        var transfer = try only(board)
        #expect(transfer.direction == .fetch)
        #expect(transfer.done == 512)
        #expect(transfer.retries == 1)
        #expect(transfer.chunkSize == 512)
        #expect(transfer.lastRetry == .fetch(stalled: true, waitMilliseconds: 40))
        // A fetch's size is unknown until its verdict.
        #expect(transfer.cells(4) == nil)

        board.apply(Fixture.at(0, .fetchVerified(blob: fetched, size: 1024)))
        transfer = try only(board)
        #expect(transfer.phase == .verified)
        #expect(transfer.cells(2) == [.verified, .verified])
    }

    @Test("A fetch that fails its hash is rejected whole")
    func integrityRejected() throws {
        let board = Fixture.board([Fixture.at(0, .integrityRejected(blob: fetched, size: 10))])
        let transfer = try only(board)
        #expect(transfer.phase == .integrityRejected)
        #expect(transfer.rejects == 1)
        #expect(transfer.cells(2) == [.rejected, .rejected])
    }

    @Test("An open-ended fetch covers the rest of the strip")
    func openEndedFetch() {
        var transfer = TransferBoard.Transfer(blob: fetched, direction: .fetch, atMilliseconds: 0)
        transfer.size = 400
        transfer.done = 100
        transfer.active = TransferBoard.ActiveChunk(offset: 100, length: nil, state: .inFlight)
        #expect(transfer.cells(4) == [.acked, .inFlight, .inFlight, .inFlight])
    }

    @Test("The strip has no more cells than bytes, and none for an empty blob")
    func stripGeometry() {
        var transfer = TransferBoard.Transfer(blob: BlobHash("b"), direction: .upload, atMilliseconds: 0)
        transfer.size = 3
        #expect(transfer.cells(10)?.count == 3)
        #expect(transfer.cells(0) == nil)
        transfer.size = 0
        #expect(transfer.cells(10) == nil)
    }

    @Test("A partly acknowledged cell is still pending")
    func partlyAcked() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(400)),
            Fixture.at(1, Fixture.sent(0, 150)),
            Fixture.at(2, Fixture.acked(0, 150)),
        ])
        #expect(try only(board).cells(4) == [.acked, .pending, .pending, .pending])
    }

    @Test("Finished transfers past the bound are dropped oldest first; a live one never is")
    func finishedBound() {
        var board = TransferBoard()
        for index in 0 ..< (TransferBoard.finishedKept + 3) {
            board.apply(Fixture.at(UInt64(index), .fetchVerified(blob: BlobHash("f\(index)"), size: 1)))
        }
        board.apply(Fixture.at(100, Fixture.sent(0, 1)))
        #expect(board.transfers.count == TransferBoard.finishedKept + 1)
        #expect(board.transfers.first?.blob == BlobHash("f3"))
    }

    @Test("An unfinished transfer silent for the stale interval ages out")
    func staleAgesOut() {
        let stale = TransferBoard.staleAfterMilliseconds
        var board = TransferBoard()
        board.apply(Fixture.at(0, createdBlob("quiet")))
        board.apply(Fixture.at(1, createdBlob("busy")))
        board.tick(to: stale - 1)
        #expect(board.transfers.count == 2)
        // An event keeps one alive; the other crosses the interval on a tick.
        board.apply(Fixture.at(stale - 1, createdBlob("busy")))
        board.tick(to: stale)
        #expect(board.transfers.count == 1)
        board.tick(to: 2 * stale)
        #expect(board.transfers.isEmpty)
    }

    @Test("A finalizing upload whose receipt never came ages out")
    func finalizingAgesOut() throws {
        var board = Fixture.board([
            Fixture.at(0, Fixture.created(kib)),
            Fixture.at(2, .finalize(blob: Fixture.blob, session: Fixture.session, size: kib)),
        ])
        #expect(try only(board).phase == .finalizing)
        board.tick(to: 2 + TransferBoard.staleAfterMilliseconds)
        #expect(board.transfers.isEmpty)
    }

    @Test("An event after ageing out starts a fresh transfer")
    func freshAfterAgeing() throws {
        let stale = TransferBoard.staleAfterMilliseconds
        var board = Fixture.board([Fixture.at(0, Fixture.created(kib))])
        board.tick(to: stale)
        board.apply(Fixture.at(stale + 1, Fixture.sent(0, 1)))
        #expect(try only(board).startedMilliseconds == stale + 1)
    }

    @Test("Never-finished transfers keep the board bounded, by the cap and by age")
    func boundedForever() {
        let kept = TransferBoard.transfersKept
        var board = TransferBoard()
        for index in 0 ..< (kept * 4) {
            board.apply(Fixture.at(UInt64(index), createdBlob("c\(index)")))
            #expect(board.transfers.count <= kept)
        }
        #expect(board.transfers.count == kept)
        // The ones silent longest were the ones dropped.
        #expect(board.transfers.first?.blob == BlobHash("c\(kept * 3)"))

        var aged = TransferBoard()
        let spacing = TransferBoard.staleAfterMilliseconds / 8
        var peak = 0
        for index in UInt64(0) ..< 2000 {
            aged.apply(Fixture.at(index * spacing, createdBlob("a\(index)")))
            peak = max(peak, aged.transfers.count)
        }
        #expect(peak <= 8)
        #expect(aged.activeCount <= 8)
    }

    @Test("Scars are bounded; retries are still all counted")
    func scarsBounded() throws {
        var events = [Fixture.at(0, Fixture.created(kib))]
        for index in 0 ..< (TransferBoard.scarsKept + 5) {
            events.append(Fixture.at(UInt64(index), Fixture.retried(0, 1, .serverWait(waitMilliseconds: 0))))
        }
        let board = Fixture.board(events)
        let transfer = try only(board)
        #expect(transfer.scars.count == TransferBoard.scarsKept)
        #expect(transfer.retries == TransferBoard.scarsKept + 5)
    }

    @Test("Chunk size history is bounded to the latest sizes")
    func chunkHistoryBounded() throws {
        var events = [Fixture.at(0, Fixture.created(100000 * kib))]
        var offset: UInt64 = 0
        for (index, length) in [UInt64(4), 8, 16, 32, 64, 128].enumerated() {
            events.append(Fixture.at(UInt64(index) + 1, Fixture.sent(offset, length * kib)))
            offset += length * kib
        }
        let transfer = try only(Fixture.board(events))
        #expect(transfer.chunkSizeChanges == 5)
        #expect(transfer.chunkSizes.count == TransferBoard.chunkSizesKept)
        #expect(transfer.chunkSize == 128 * kib)
    }
}
