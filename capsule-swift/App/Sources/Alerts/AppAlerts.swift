import CapsuleAlerts
import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleNavigation
import CapsulePorts

// MARK: - Composition

extension AppEnvironment {
    /// The alert loop over this world's ports.
    ///
    /// The mock lane composes the in-memory scheduler and a scripted
    /// permission prompt, for the reason it composes `MockLocalAuthenticator`:
    /// a system prompt over a mocked world is exactly what this composition
    /// root keeps out. The system adapters — `UserNotificationAlertScheduler`
    /// and `UserNotificationAuthorization` — are the swap, beside the
    /// Rust-backed `AlertPolicyPort`, when the SDK adapter lands.
    ///
    /// The clock is the mock world's, not the wall clock: every fact in this
    /// world is dated against it, and a deadline measured against another
    /// clock would be weeks wrong.
    static func makeAlerts(mock: MockEnvironment) -> AlertCoordinator {
        let clock = mock.configuration.clock
        return AlertCoordinator(
            facts: PortAlertFacts(
                sync: mock.sync,
                quota: mock.quota,
                quarantine: mock.quarantine,
                drops: mock.drops,
                recovery: mock.recovery,
                settings: mock.settings
            ),
            policy: MockAlertPolicy(),
            scheduler: InMemoryAlertScheduler(),
            authorization: MockNotificationAuthorization(),
            snoozes: UserDefaultsAlertSnoozeStore(),
            clock: AlertClock { clock.now }
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
        let settings = settings
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await Self.refresh(alerts, onEach: sync.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: quota.changes()) }
            group.addTask { await Self.refresh(alerts, onEach: quarantine.changes()) }
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
