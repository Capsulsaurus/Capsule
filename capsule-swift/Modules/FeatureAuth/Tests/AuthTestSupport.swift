import CapsuleDomain
import Foundation

// MARK: - Instants

/// The instant every suite in this target measures against.
///
/// A literal rather than `Date()`: half of what these screens do is arithmetic
/// on deadlines — a ten-minute enrollment code, a 90-day verification interval,
/// a 180-day session expiry — and a test that cannot pin "now" can only assert
/// that a countdown is *some* number.
enum AuthInstant {
    /// 2026-08-22T12:00:00Z, the same anchor `CapsuleMock` uses, so a stub and a
    /// mock world in the same test agree about what time it is.
    static let reference = CapsuleTimestamp(epochSeconds: 1787400000)

    /// An instant a whole number of days from ``reference``.
    static func days(_ count: Int) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: reference.epochSeconds + Int64(count) * 86400)
    }

    /// An instant a whole number of seconds from ``reference``.
    static func seconds(_ count: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: reference.epochSeconds + count)
    }

    /// A frozen clock in the shape the view models take.
    static let frozen: @Sendable () -> CapsuleTimestamp = { reference }
}

// MARK: - Waiting

/// Thrown when a polled condition never became true.
struct ConditionTimedOut: Error, CustomStringConvertible {
    let description: String
}

/// Wait for a main-actor condition, bounded by a real deadline.
///
/// A few of these view models hand work to a detached `Task` that hops back to
/// the main actor — an enrollment relay's progress stream, chiefly — so there is
/// genuinely nothing to `await`. Polling against a `ContinuousClock` deadline is
/// the honest way to observe that: it fails loudly at a fixed wall-clock bound
/// instead of hanging, and it does not depend on how many actor hops the
/// runtime happens to need.
@MainActor
func waitUntil(
    _ description: String,
    within duration: Duration = .seconds(5),
    _ condition: @MainActor () -> Bool
) async throws {
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: duration)
    while !condition() {
        if clock.now >= deadline {
            throw ConditionTimedOut(description: "timed out waiting until \(description)")
        }
        try await Task.sleep(for: .milliseconds(1))
    }
}

/// Assert that a main-actor condition stays true across a settling window.
///
/// The counterpart to ``waitUntil(_:within:_:)``, for the negative claims: "the
/// ceremony does **not** complete when the far side finishes early" is a
/// statement about something that must not happen, and the only honest way to
/// observe it is to keep looking for a bounded while.
@MainActor
func holdsThroughout(
    _ description: String,
    for duration: Duration = .milliseconds(100),
    _ condition: @MainActor () -> Bool
) async throws {
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: duration)
    while clock.now < deadline {
        if !condition() {
            throw ConditionTimedOut(description: "\(description) stopped holding")
        }
        try await Task.sleep(for: .milliseconds(1))
    }
}

// MARK: - EventRelay

/// A hand-cranked `AsyncStream` source.
///
/// The continuation is captured synchronously inside `AsyncStream`'s build
/// closure and anything emitted before a consumer arrived is buffered, so a
/// test can drive a ceremony one event at a time without racing the view model
/// that subscribes to it. That is the whole reason it exists: the alternative
/// is sleeping and hoping.
final class EventRelay<Element: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: AsyncStream<Element>.Continuation?
    private var pending: [Element] = []
    private var isFinished = false

    /// A stream that replays anything emitted before it was asked for.
    func stream() -> AsyncStream<Element> {
        AsyncStream { continuation in
            lock.lock()
            for element in pending {
                continuation.yield(element)
            }
            pending.removeAll()
            if isFinished {
                continuation.finish()
            } else {
                self.continuation = continuation
            }
            lock.unlock()
        }
    }

    func emit(_ element: Element) {
        lock.lock()
        let target = continuation
        if target == nil { pending.append(element) }
        lock.unlock()
        target?.yield(element)
    }

    func finish() {
        lock.lock()
        let target = continuation
        isFinished = true
        continuation = nil
        lock.unlock()
        target?.finish()
    }
}

// MARK: - Ledger fixtures

/// Device and session rows, built by hand so a cohort test states its own
/// grouping rather than inheriting one from a scenario.
enum LedgerFixture {
    static func device(
        ordinal: Int,
        cohort: String?,
        lastSeenDays: Int,
        isCurrent: Bool = false,
        revokedDays: Int? = nil
    ) -> DeviceRecord {
        DeviceRecord(
            id: DeviceID("device-\(ordinal)"),
            model: "Model-\(ordinal)",
            platform: .ios,
            firstSeen: AuthInstant.days(-400),
            lastSeen: AuthInstant.days(lastSeenDays),
            cohortHash: cohort,
            isCurrent: isCurrent,
            revokedAt: revokedDays.map { AuthInstant.days($0) }
        )
    }

    static func session(
        ordinal: Int,
        cohort: String?,
        lastUsedDays: Int,
        isCurrent: Bool = false,
        inactivityDays: Int = 180,
        hardDays: Int = 300,
        revokedDays: Int? = nil
    ) -> SessionRecord {
        SessionRecord(
            id: SessionID("session-\(ordinal)"),
            deviceID: DeviceID("device-\(ordinal)"),
            cohortHash: cohort,
            createdAt: AuthInstant.days(-400),
            lastUsedAt: AuthInstant.days(lastUsedDays),
            inactivityExpiresAt: AuthInstant.days(inactivityDays),
            hardExpiresAt: AuthInstant.days(hardDays),
            isCurrent: isCurrent,
            revokedAt: revokedDays.map { AuthInstant.days($0) }
        )
    }
}
