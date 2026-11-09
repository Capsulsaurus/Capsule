import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Foundation

@testable import CapsuleAlerts

/// 2026-01-01T00:00:00Z.
let epoch = CapsuleTimestamp(epochSeconds: 1767225600)

func day(_ days: Int64, plusSeconds seconds: Int64 = 0) -> CapsuleTimestamp {
    CapsuleTimestamp(epochSeconds: epoch.epochSeconds + days * 86400 + seconds)
}

/// A clock a test moves by hand.
final class TestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current: CapsuleTimestamp

    init(_ start: CapsuleTimestamp = epoch) {
        current = start
    }

    var now: CapsuleTimestamp {
        lock.withLock { current }
    }

    func set(_ instant: CapsuleTimestamp) {
        lock.withLock { current = instant }
    }

    var alertClock: AlertClock {
        AlertClock { [self] in now }
    }
}

/// Facts a test edits between refreshes.
actor StubFacts: AlertFactsPort {
    private var input: NotifyInput

    init(_ input: NotifyInput = NotifyInput()) {
        self.input = input
    }

    func facts() async -> NotifyInput {
        input
    }

    func set(_ input: NotifyInput) {
        self.input = input
    }
}

/// A predicate that always refuses its input.
struct RefusingPolicy: AlertPolicyPort {
    struct Refused: Error {}

    func evaluate(_: NotifyInput, at _: CapsuleTimestamp) throws -> [Alert] {
        throw Refused()
    }

    func preArmDeadlines(_: NotifyInput, at _: CapsuleTimestamp) throws -> [ClassDeadline] {
        throw Refused()
    }
}

/// One coordinator wired over in-memory adapters, with handles on each.
struct Harness {
    let clock: TestClock
    let facts: StubFacts
    let scheduler: InMemoryAlertScheduler
    let authorization: MockNotificationAuthorization
    let snoozes: InMemoryAlertSnoozeStore
    let coordinator: AlertCoordinator

    init(
        _ input: NotifyInput = NotifyInput(),
        at start: CapsuleTimestamp = epoch,
        authorization: MockNotificationAuthorization = MockNotificationAuthorization(),
        scheduler: InMemoryAlertScheduler = InMemoryAlertScheduler(),
        policy: any AlertPolicyPort = MockAlertPolicy()
    ) {
        clock = TestClock(start)
        facts = StubFacts(input)
        self.scheduler = scheduler
        self.authorization = authorization
        snoozes = InMemoryAlertSnoozeStore()
        coordinator = AlertCoordinator(
            facts: facts,
            policy: policy,
            scheduler: scheduler,
            authorization: authorization,
            snoozes: snoozes,
            clock: clock.alertClock
        )
    }

    /// When each class's one alarm fires, or nothing for an unarmed class.
    func armedInstants() async -> [AlertClass: CapsuleTimestamp] {
        await scheduler.armed().mapValues(\.fireAt)
    }

    /// The most live alarms any one class ever held, replayed from the
    /// scheduler's log — the "never two live timers per class" assertion.
    func maximumLivePerClass() async -> Int {
        var live: [AlertClass: Int] = [:]
        var peak = 0
        for event in await scheduler.log {
            switch event {
            case let .armed(alarm):
                // Arming over a live alarm replaces it in the OS; the plan must
                // still have cancelled it first.
                live[alarm.alertClass, default: 0] += 1
            case let .cancelled(alertClass):
                live[alertClass, default: 0] -= 1
            }
            peak = max(peak, live.values.max() ?? 0)
        }
        return peak
    }
}
