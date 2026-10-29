import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import Testing

@testable import FeatureTransfer

/// The mock lane's recorded stream folds into the board the transfer view
/// previews: every state the view draws is reached, and the stream replays
/// into the same board every time.
@Suite("Transfer board: the demo feed")
struct TransferBoardDemoFeedTests {
    private func fold(_ feed: MockTransferEventFeed) async -> TransferBoard {
        var board = TransferBoard()
        for await item in feed.events() {
            board.apply(item)
        }
        return board
    }

    @Test("The demo upload resumes, resizes, retries, is refused once, and is verified by a receipt")
    func demoUpload() async throws {
        let board = await fold(.demo(seed: 1))
        let upload = try #require(board.transfers.first { $0.direction == .upload })
        #expect(upload.phase == .verified)
        #expect(upload.held > 0)
        #expect(upload.retries == 1)
        #expect(upload.rejects == 1)
        #expect(upload.chunkSizeChanges >= 1)
        #expect(upload.receiptSequence == 1)
        #expect(board.custody == TransferBoard.Custody(durable: 1, notDurable: 0, receipts: 1))
    }

    @Test("The demo fetch stalls once and verifies")
    func demoFetch() async throws {
        let board = await fold(.demo(seed: 1))
        let fetch = try #require(board.transfers.first { $0.direction == .fetch })
        #expect(fetch.phase == .verified)
        #expect(fetch.lastRetry == .fetch(stalled: true, waitMilliseconds: 500))
        #expect(board.activeCount == 0)
    }

    @Test("The stream is in stamp order and replays into the same board")
    func deterministic() async {
        let feed = MockTransferEventFeed.demo(seed: 1)
        let stamps = feed.script.compactMap { item -> UInt64? in
            guard case let .event(stamped) = item else { return nil }
            return stamped.atMilliseconds
        }
        #expect(stamps == stamps.sorted())
        let first = await fold(feed)
        let second = await fold(feed)
        #expect(first == second)
    }
}
