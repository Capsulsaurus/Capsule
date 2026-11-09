import CapsuleDomain
import CapsuleFoundation
import Testing

@testable import FeatureTransfer

/// The board's fold over an upload: progress, the chunk in flight, scars,
/// resume, adaptive chunk size, throughput and custody. The same cases the
/// TUI's transfer view pins, so the two surfaces draw one stream alike.
@Suite("Transfer board: uploads")
struct TransferBoardUploadTests {
    private typealias Fixture = BoardFixture
    private let kib = BoardFixture.kib

    private func only(_ board: TransferBoard) throws -> TransferBoard.Transfer {
        try #require(board.transfers.count == 1)
        return board.transfers[0]
    }

    @Test("A new board is empty")
    func emptyBoard() {
        let board = TransferBoard()
        #expect(board.transfers.isEmpty)
        #expect(board.throughput == 0)
        #expect(board.activeCount == 0)
        #expect(board.missed == 0)
        #expect(board.nowMilliseconds == 0)
    }

    @Test("A clean upload moves its prefix and finalizes")
    func cleanUpload() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(512 * kib)),
            Fixture.at(10, Fixture.sent(0, 256 * kib)),
            Fixture.at(20, Fixture.acked(0, 256 * kib)),
            Fixture.at(30, Fixture.sent(256 * kib, 256 * kib)),
            Fixture.at(40, Fixture.acked(256 * kib, 256 * kib)),
            Fixture.at(50, .finalize(blob: Fixture.blob, session: Fixture.session, size: 512 * kib)),
        ])
        let transfer = try only(board)
        #expect(transfer.done == 512 * kib)
        #expect(transfer.moved == 512 * kib)
        #expect(transfer.active == nil)
        #expect(transfer.phase == .finalizing)
        #expect(transfer.held == 0)
        // Finalizing is still live: a receipt has yet to verify it.
        #expect(board.activeCount == 1)
    }

    @Test("A short write leaves the prefix at what the server kept")
    func shortWrite() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(512 * kib)),
            Fixture.at(1, Fixture.sent(0, 256 * kib)),
            Fixture.at(2, .chunkAcked(
                blob: Fixture.blob, session: Fixture.session, offset: 0, length: 256 * kib, ackedOffset: 100 * kib
            )),
        ])
        let transfer = try only(board)
        #expect(transfer.done == 100 * kib)
        #expect(transfer.moved == 100 * kib)
    }

    @Test("A receipt verifies the upload and counts toward custody")
    func receiptVerifies() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(kib)),
            Fixture.at(5, .finalize(blob: Fixture.blob, session: Fixture.session, size: kib)),
            Fixture.at(6, .verify(asset: AssetID.managed(uuid: "a1"), durable: true, blobs: 1, notDurable: 0)),
            Fixture.at(7, .receipt(asset: AssetID.managed(uuid: "a1"), blob: Fixture.blob, receiptSequence: 4)),
        ])
        let transfer = try only(board)
        #expect(transfer.phase == .verified)
        #expect(transfer.receiptSequence == 4)
        #expect(board.activeCount == 0)
        #expect(board.custody == TransferBoard.Custody(durable: 1, notDurable: 0, receipts: 1))
    }

    @Test("A receipt for a blob never seen uploading is counted and nothing more")
    func receiptForUnseenBlob() {
        let board = Fixture.board([
            Fixture.at(0, .receipt(asset: AssetID.managed(uuid: "a1"), blob: BlobHash("elsewhere"), receiptSequence: 1)),
            Fixture.at(1, .verify(asset: AssetID.managed(uuid: "a1"), durable: false, blobs: 2, notDurable: 1)),
        ])
        #expect(board.transfers.isEmpty)
        #expect(board.custody.receipts == 1)
        #expect(board.custody.notDurable == 1)
    }

    @Test("A retry marks the chunk and leaves a scar after it lands")
    func retryScar() throws {
        var board = Fixture.board([
            Fixture.at(0, Fixture.created(512 * kib)),
            Fixture.at(1, Fixture.sent(0, 256 * kib)),
            Fixture.at(2, Fixture.retried(0, 256 * kib, .transportReset(waitMilliseconds: 250))),
        ])
        var transfer = try only(board)
        #expect(transfer.retries == 1)
        #expect(transfer.lastRetry == .upload(.transportReset(waitMilliseconds: 250)))
        #expect(transfer.cells(2) == [.retrying, .pending])

        board.apply(Fixture.at(3, Fixture.sent(0, 256 * kib)))
        board.apply(Fixture.at(4, Fixture.acked(0, 256 * kib)))
        transfer = try only(board)
        #expect(transfer.cells(2) == [.ackedAfterRetry, .pending])
    }

    @Test("A checksum refusal counts, marks the chunk, and outranks a retry scar")
    func checksumRejection() throws {
        var board = Fixture.board([
            Fixture.at(0, Fixture.created(512 * kib)),
            Fixture.at(1, Fixture.sent(0, 256 * kib)),
            Fixture.at(2, .checksumRejected(blob: Fixture.blob, session: Fixture.session, offset: 0, length: 256 * kib)),
        ])
        #expect(try only(board).rejects == 1)
        #expect(try only(board).cells(2) == [.rejected, .pending])
        board.apply(Fixture.at(3, Fixture.sent(0, 256 * kib)))
        #expect(try only(board).cells(2) == [.inFlight, .pending])
        board.apply(Fixture.at(4, Fixture.acked(0, 256 * kib)))
        #expect(try only(board).cells(2) == [.ackedAfterReject, .pending])
    }

    @Test("An offset realign moves the prefix back")
    func offsetRealign() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(1024 * kib)),
            Fixture.at(1, Fixture.sent(0, 512 * kib)),
            Fixture.at(2, Fixture.acked(0, 512 * kib)),
            Fixture.at(3, Fixture.sent(512 * kib, 512 * kib)),
            Fixture.at(4, Fixture.retried(512 * kib, 512 * kib, .offsetRealign(toOffset: 256 * kib))),
        ])
        let transfer = try only(board)
        #expect(transfer.done == 256 * kib)
        #expect(transfer.cells(4) == [.acked, .pending, .retrying, .retrying])
    }

    @Test("A resumed upload shows what the server already held")
    func resumedUpload() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(1024 * kib, resume: 512 * kib)),
            Fixture.at(1, Fixture.sent(512 * kib, 256 * kib)),
            Fixture.at(2, Fixture.acked(512 * kib, 256 * kib)),
        ])
        let transfer = try only(board)
        #expect(transfer.held == 512 * kib)
        #expect(transfer.resumes == 1)
        #expect(transfer.resumedAt == 512 * kib)
        #expect(transfer.moved == 256 * kib)
        #expect(transfer.cells(4) == [.held, .held, .acked, .pending])
    }

    @Test("A resume after a cut continues the same transfer, and this run's bytes are not held")
    func resumeAfterCut() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(1024 * kib)),
            Fixture.at(1, Fixture.sent(0, 256 * kib)),
            Fixture.at(2, Fixture.acked(0, 256 * kib)),
            Fixture.at(3, Fixture.sent(256 * kib, 256 * kib)),
            Fixture.at(4, .sessionResumed(blob: Fixture.blob, session: Fixture.session, size: 1024 * kib, offset: 256 * kib)),
        ])
        let transfer = try only(board)
        #expect(transfer.resumes == 1)
        #expect(transfer.sessions == 1)
        #expect(transfer.active == nil)
        #expect(transfer.held == 0)
        #expect(transfer.done == 256 * kib)
    }

    @Test("A lost session reopens on the same transfer")
    func lostSession() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(1024 * kib)),
            Fixture.at(1, Fixture.sent(0, 256 * kib)),
            Fixture.at(2, Fixture.acked(0, 256 * kib)),
            Fixture.at(3, Fixture.sent(256 * kib, 256 * kib)),
            Fixture.at(4, Fixture.retried(256 * kib, 256 * kib, .sessionLost)),
            Fixture.at(5, Fixture.created(1024 * kib, resume: 128 * kib, session: UploadID("s2"))),
        ])
        let transfer = try only(board)
        #expect(transfer.sessions == 2)
        #expect(transfer.session == UploadID("s2"))
        #expect(transfer.done == 128 * kib)
        #expect(transfer.resumedAt == 128 * kib)
    }

    @Test("An upload after finalizing starts a new transfer")
    func uploadAfterFinalize() {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(kib)),
            Fixture.at(1, .finalize(blob: Fixture.blob, session: Fixture.session, size: kib)),
            Fixture.at(2, Fixture.created(kib, session: UploadID("s2"))),
        ])
        #expect(board.transfers.count == 2)
    }

    @Test("An event whose create was missed still gets a transfer")
    func eventWithoutCreate() throws {
        let board = Fixture.board([Fixture.at(0, Fixture.sent(0, 4 * kib))])
        let transfer = try only(board)
        #expect(transfer.size == nil)
        #expect(transfer.cells(10) == nil)
        #expect(transfer.chunkSize == 4 * kib)
    }

    @Test("A merge sends nothing and finishes the transfer")
    func merge() throws {
        let board = Fixture.board([Fixture.at(0, .merged(blob: Fixture.blob, assetRef: "a1"))])
        #expect(try only(board).phase == .merged)
        #expect(board.activeCount == 0)
    }

    @Test("Chunk resizes are counted and the tail is not one")
    func chunkResizes() throws {
        let board = Fixture.board([
            Fixture.at(0, Fixture.created(1000 * kib)),
            Fixture.at(1, Fixture.sent(0, 128 * kib)),
            Fixture.at(2, Fixture.acked(0, 128 * kib)),
            Fixture.at(3, Fixture.sent(128 * kib, 128 * kib)),
            Fixture.at(4, Fixture.acked(128 * kib, 128 * kib)),
            Fixture.at(5, Fixture.sent(256 * kib, 256 * kib)),
            Fixture.at(6, Fixture.acked(256 * kib, 256 * kib)),
            Fixture.at(7, Fixture.sent(512 * kib, 128 * kib)),
            Fixture.at(8, Fixture.acked(512 * kib, 128 * kib)),
            // The tail: what remains, not a resize.
            Fixture.at(9, Fixture.sent(640 * kib, 360 * kib)),
        ])
        let transfer = try only(board)
        #expect(transfer.chunkSizeChanges == 2)
        #expect(transfer.chunkSizes == [256 * kib, 128 * kib])
        #expect(transfer.chunkSize == 128 * kib)
    }

    @Test("Throughput is the window's average and decays on ticks; the clock never goes back")
    func throughputWindow() {
        var board = Fixture.board([
            Fixture.at(0, Fixture.created(8192 * kib)),
            Fixture.at(500, Fixture.acked(0, 1024 * kib)),
            Fixture.at(1500, Fixture.acked(1024 * kib, 1024 * kib)),
        ])
        // 2 MiB over a 2 s window: 1 MiB/s.
        #expect(board.throughput == 1024 * kib)
        board.tick(to: 2500)
        // The first sample (500 ms) has left the window (500, 2500].
        #expect(board.throughput == 512 * kib)
        board.tick(to: 10000)
        #expect(board.throughput == 0)
        board.tick(to: 5)
        #expect(board.nowMilliseconds == 10000)
    }

    @Test("Missed events are counted")
    func lagCounted() {
        var board = TransferBoard()
        board.apply(.lagged(3))
        board.apply(.lagged(4))
        #expect(board.missed == 7)
    }
}
