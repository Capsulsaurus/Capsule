import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - LiveTransferClock

/// The clock the real-time transfer view paces and ages its board by.
///
/// Injected so a test replays a recorded stream without waiting: the board
/// itself never reads a clock, and this is the one place the view does.
public struct LiveTransferClock: Sendable {
    /// The current instant on a monotonic clock.
    public var now: @Sendable () -> ContinuousClock.Instant
    /// Wait for `duration`; returns early when the task is cancelled.
    public var sleep: @Sendable (Duration) async -> Void

    public init(
        now: @escaping @Sendable () -> ContinuousClock.Instant,
        sleep: @escaping @Sendable (Duration) async -> Void
    ) {
        self.now = now
        self.sleep = sleep
    }

    /// The system's monotonic clock. The default for the app.
    public static let continuous = LiveTransferClock(
        now: { ContinuousClock.now },
        sleep: { duration in try? await Task.sleep(for: duration) }
    )

    /// Milliseconds from `origin` to now, never negative.
    func milliseconds(since origin: ContinuousClock.Instant) -> UInt64 {
        let elapsed = now() - origin
        guard elapsed > .zero else { return 0 }
        let (seconds, attoseconds) = elapsed.components
        return UInt64(seconds) * 1000 + UInt64(attoseconds / 1000000000000000)
    }
}

// MARK: - LiveTransferModel

/// Drives the real-time transfer view: subscribes to ``TransferEventPort`` and
/// folds every item into a ``TransferBoard``.
///
/// The board is what the view draws, and it starts afresh each time the view
/// starts watching, because a subscription only sees what is published after
/// it opens.
///
/// **Pacing.** An event is folded once the view's own clock has reached its
/// stamp. A live adapter stamps events as they arrive, so they fold at once; a
/// recorded stream (the mock lane's) arrives all at once and is replayed at the
/// pace it was recorded, so the strip moves the way it moved live.
@MainActor
@Observable
public final class LiveTransferModel {
    /// What the view draws.
    public private(set) var board = TransferBoard()
    /// Whether a subscription is open.
    public private(set) var isWatching = false
    /// Whether the stream ended: the publisher is gone, and nothing more will
    /// arrive until the view watches again.
    public private(set) var hasEnded = false

    private let port: any TransferEventPort
    private let clock: LiveTransferClock
    private var origin: ContinuousClock.Instant?

    public init(port: any TransferEventPort, clock: LiveTransferClock = .continuous) {
        self.port = port
        self.clock = clock
    }

    /// Transfers to draw, newest activity first.
    public var transfers: [TransferBoard.Transfer] {
        board.transfers.sorted { $0.lastMilliseconds > $1.lastMilliseconds }
    }

    /// Subscribe and fold until the stream ends or the calling task is
    /// cancelled. Refused while already watching.
    public func watch() async {
        guard !isWatching else { return }
        isWatching = true
        hasEnded = false
        board = TransferBoard()
        let origin = clock.now()
        self.origin = origin
        CapsuleLog.interface.debug("live transfer view: subscription opened")
        defer {
            isWatching = false
            self.origin = nil
        }
        for await item in port.events() {
            if case let .event(stamped) = item {
                let elapsed = clock.milliseconds(since: origin)
                if stamped.atMilliseconds > elapsed {
                    await clock.sleep(.milliseconds(Int64(clamping: stamped.atMilliseconds - elapsed)))
                }
            }
            if Task.isCancelled { break }
            board.apply(item)
        }
        hasEnded = !Task.isCancelled
        let held = board.transfers.count
        CapsuleLog.interface.debug("live transfer view: subscription closed, transfers=\(held, privacy: .public)")
    }

    /// Move the board's clock to now, so throughput decays while nothing moves
    /// and a silent transfer ages out. A no-op while not watching.
    public func tick() {
        guard let origin else { return }
        board.tick(to: clock.milliseconds(since: origin))
    }

    /// Tick once a second until the calling task is cancelled.
    public func runClock() async {
        while !Task.isCancelled {
            await clock.sleep(.seconds(1))
            tick()
        }
    }
}
