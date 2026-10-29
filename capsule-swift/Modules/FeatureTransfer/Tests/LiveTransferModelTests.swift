import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import Synchronization
import Testing

@testable import FeatureTransfer

/// A clock that only moves when the model sleeps on it, and records each
/// sleep, so pacing is asserted without waiting.
private final class SteppedClock: Sendable {
    let base = ContinuousClock.now
    private let elapsed = Mutex<Duration>(.zero)
    private let slept = Mutex<[Duration]>([])

    var clock: LiveTransferClock {
        LiveTransferClock(
            now: { [self] in base + elapsed.withLock { $0 } },
            sleep: { [self] duration in
                elapsed.withLock { $0 += duration }
                slept.withLock { $0.append(duration) }
            }
        )
    }

    var sleeps: [Duration] { slept.withLock { $0 } }

    func advance(by duration: Duration) {
        elapsed.withLock { $0 += duration }
    }
}

/// The real-time transfer view's model folds the stream it subscribes to into
/// the board, paced to the stamps, and ages the board on its own clock.
@MainActor
@Suite("Live transfer model")
struct LiveTransferModelTests {
    private func fold(_ feed: MockTransferEventFeed) async -> TransferBoard {
        var board = TransferBoard()
        for await item in feed.events() {
            board.apply(item)
        }
        return board
    }

    @Test("Watching folds the whole stream into the board the stream folds into")
    func watchFolds() async {
        let feed = MockTransferEventFeed.demo(seed: 1)
        let model = LiveTransferModel(port: feed, clock: SteppedClock().clock)
        await model.watch()
        let expected = await fold(feed)
        #expect(model.board == expected)
        #expect(model.hasEnded)
        #expect(!model.isWatching)
    }

    @Test("A recorded stream is replayed at the pace it was recorded")
    func pacing() async {
        let feed = MockTransferEventFeed(script: [
            .event(StampedTransferEvent(atMilliseconds: 0, event: .rangeRequested(blob: BlobHash("b"), offset: 0, maxLength: 8))),
            .event(StampedTransferEvent(atMilliseconds: 250, event: .rangeReceived(blob: BlobHash("b"), offset: 0, length: 8))),
            .event(StampedTransferEvent(atMilliseconds: 400, event: .fetchVerified(blob: BlobHash("b"), size: 8))),
        ])
        let stepped = SteppedClock()
        let model = LiveTransferModel(port: feed, clock: stepped.clock)
        await model.watch()
        #expect(stepped.sleeps == [.milliseconds(250), .milliseconds(150)])
        #expect(model.board.nowMilliseconds == 400)
        #expect(model.board.transfers.first?.phase == .verified)
    }

    @Test("Missed events are counted")
    func lagged() async {
        let feed = MockTransferEventFeed(script: [.lagged(3), .lagged(2)])
        let model = LiveTransferModel(port: feed, clock: SteppedClock().clock)
        await model.watch()
        #expect(model.board.missed == 5)
        #expect(model.transfers.isEmpty)
    }

    @Test("Transfers are listed newest activity first")
    func ordering() async {
        let feed = MockTransferEventFeed(script: [
            .event(StampedTransferEvent(atMilliseconds: 0, event: .rangeRequested(blob: BlobHash("a"), offset: 0, maxLength: 8))),
            .event(StampedTransferEvent(atMilliseconds: 10, event: .rangeRequested(blob: BlobHash("b"), offset: 0, maxLength: 8))),
        ])
        let model = LiveTransferModel(port: feed, clock: SteppedClock().clock)
        await model.watch()
        #expect(model.transfers.map(\.blob) == [BlobHash("b"), BlobHash("a")])
    }

    @Test("Ticking ages the board only while watching")
    func ticking() async {
        let stepped = SteppedClock()
        let model = LiveTransferModel(port: MockTransferEventFeed(script: []), clock: stepped.clock)
        stepped.advance(by: .seconds(5))
        model.tick()
        #expect(model.board.nowMilliseconds == 0)
    }

    @Test("Every phase, direction, cell and retry note is named")
    func presentation() {
        let phases: [TransferBoard.Phase] = [.active, .finalizing, .verified, .merged, .integrityRejected]
        #expect(Set(phases.map(\.badge.titleKey)).count == phases.count)
        #expect(TransferBoard.Direction.upload.badge.titleKey != TransferBoard.Direction.fetch.badge.titleKey)
        let notes: [TransferBoard.RetryNote] = [
            .upload(.transportReset(waitMilliseconds: 1)),
            .upload(.offsetRealign(toOffset: 2)),
            .upload(.sessionLost),
            .upload(.serverWait(waitMilliseconds: 3)),
            .fetch(stalled: true, waitMilliseconds: 4),
            .fetch(stalled: false, waitMilliseconds: 5),
        ]
        #expect(Set(notes.map(\.titleKey)).count == notes.count)
    }
}
