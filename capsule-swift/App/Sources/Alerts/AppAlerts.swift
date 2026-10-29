import CapsuleAlerts
import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Foundation
#if canImport(CapsuleCatalogFFI)
    import CapsuleCatalogFFI
#endif

// MARK: - Adapters

/// Which adapters the alert loop runs on.
///
/// The two lanes differ in exactly the three seams that reach outside the
/// process — the predicate, the OS scheduler and the permission prompt — and
/// in the clock, because the clock has to be the one the scheduler fires on.
enum AlertAdapters {
    /// The mock lane: `MockAlertPolicy`, the in-memory scheduler and a
    /// scripted permission prompt, measured against the mock world's clock.
    ///
    /// Composed for the reason the lane composes `MockLocalAuthenticator`: a
    /// system prompt over a mocked world is exactly what the mock lane keeps
    /// out, and a build with no Rust core has no other predicate to call.
    case mock
    /// The system lane: `policy` — the core's predicate over uniffi —
    /// `UserNotificationAlertScheduler` and `UserNotificationAuthorization`,
    /// measured against the wall clock.
    case system(policy: any AlertPolicyPort)

    /// The adapters this build launches with: the system lane wherever the
    /// Rust core is linked (`TUIST_FFI=1`), the mock lane everywhere else.
    static var launch: AlertAdapters {
        #if canImport(CapsuleCatalogFFI)
            .system(policy: FFIAlertPolicy())
        #else
            .mock
        #endif
    }

    /// Whether alarms are armed with the OS, and taps arrive from it.
    var armsWithSystem: Bool {
        switch self {
        case .mock: false
        case .system: true
        }
    }

    /// The instant the launch world's facts are dated against.
    ///
    /// The system lane dates the world at the launch instant: the OS fires on
    /// the wall clock, so a deadline computed from facts dated at the mock
    /// lane's fixed reference instant would be armed in the past and never
    /// fire. The mock lane keeps the reference instant, which is what makes
    /// its screens and tests reproducible.
    func worldClock(launchedAt now: Date = Date()) -> MockClock {
        switch self {
        case .mock: .reference
        case .system: MockClock(now: CapsuleTimestamp(epochSeconds: Int64(now.timeIntervalSince1970)))
        }
    }

    /// The clock the alert loop measures against: always the clock its
    /// scheduler fires on.
    func alertClock(world: MockEnvironment) -> AlertClock {
        switch self {
        case .mock:
            // The mock world's, not the wall clock: every fact in this world
            // is dated against it, and a deadline measured against another
            // clock would be weeks wrong.
            let clock = world.configuration.clock
            return AlertClock { clock.now }
        case .system:
            return .system
        }
    }
}

// MARK: - Composition

extension AppEnvironment {
    /// The alert loop over this world's ports, on `adapters`.
    static func makeAlerts(mock: MockEnvironment, adapters: AlertAdapters) -> AlertCoordinator {
        let facts = PortAlertFacts(
            sync: mock.sync,
            quota: mock.quota,
            quarantine: mock.quarantine,
            drops: mock.drops,
            recovery: mock.recovery,
            settings: mock.settings
        )
        let policy: any AlertPolicyPort
        let scheduler: any AlertSchedulerPort
        let authorization: any NotificationAuthorizationPort
        switch adapters {
        case .mock:
            policy = MockAlertPolicy()
            scheduler = InMemoryAlertScheduler()
            authorization = MockNotificationAuthorization()
        case let .system(core):
            policy = core
            scheduler = UserNotificationAlertScheduler()
            authorization = UserNotificationAuthorization()
        }
        CapsuleLog.alerts.info("alert loop composed; armed with the OS: \(adapters.armsWithSystem, privacy: .public)")
        return AlertCoordinator(
            facts: facts,
            policy: policy,
            scheduler: scheduler,
            authorization: authorization,
            snoozes: UserDefaultsAlertSnoozeStore(),
            clock: adapters.alertClock(world: mock)
        )
    }

    /// Refresh the alerts now and again after every change to a fact they
    /// read, for as long as the calling task lives.
    ///
    /// "Recompute after any state change" is the whole re-arm rule: a
    /// completed sync that is not followed by a refresh leaves the old alarm
    /// to fire into a library that is no longer stale.
    func runAlertLoop() async {
        let alerts = alerts
        await alerts.refresh()
        let sync = sync
        let quota = quota
        let quarantine = quarantine
        let drops = drops
        let settings = settings
        // Recovery has no change stream: `recovery` is wrapped in
        // `AlertRefreshingRecoveryPort`, which refreshes after each call instead.
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await Self.refresh(alerts, onEach: sync.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: quota.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: quarantine.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: drops.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: settings.changes()) }
        }
    }

    private nonisolated static func refresh(
        _ alerts: AlertCoordinator,
        onEach changes: AsyncStream<some Any>
    ) async {
        for await _ in changes {
            await alerts.refresh()
        }
    }
}

// MARK: - Taps

/// What tapping an alert does in this app.
enum AlertTapAction: Equatable {
    /// Show a screen through the router.
    case navigate(Route)
    /// Present the guided re-wrap — a fresh secret over the same master key —
    /// rather than any screen that would open on a routine check.
    case presentGuidedRewrap
}

extension AlertDestination {
    /// The app's action for a tapped alert.
    var tapAction: AlertTapAction {
        switch self {
        case .syncStatus: .navigate(.transferCenter)
        case .recoveryVerification: .navigate(.settings(.backupAndRecovery))
        case .guidedRewrap: .presentGuidedRewrap
        case .quota: .navigate(.quota)
        case .quarantine: .navigate(.quarantine)
        case .drops: .navigate(.drops)
        }
    }
}
