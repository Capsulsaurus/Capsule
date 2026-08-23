import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

/// One row of the grace-window table: how far the clock has moved, what is left,
/// and whether the view still opens.
struct GraceSample: Sendable {
    let elapsed: Int64
    let remaining: Int64
    let unlocked: Bool
}

// MARK: - LocalAuthGateTests

/// **Per-view** is the load-bearing word: unlocking Hidden must not also unlock
/// Recently Deleted, and a shared timestamp would silently make it do exactly
/// that.
@Suite("The fresh-auth grace window is per view and expires on the clock")
@MainActor
struct LocalAuthGateTests {
    @Test("exactly two views are gated, and each is named by a catalog key")
    func onlyTwoViewsAreGated() {
        #expect(GatedLibraryView.allCases == [.recentlyDeleted, .hidden])
        for view in GatedLibraryView.allCases {
            #expect(view.titleKey == "ios.settings.security.gate.\(view.rawValue)")
            #expect(!view.titleKey.contains(" "))
        }
    }

    @Test("a fresh gate holds no grants")
    func freshGateIsLocked() {
        let gate = LocalAuthGate()

        for view in GatedLibraryView.allCases {
            #expect(!gate.isUnlocked(view, at: SettingsInstant.reference))
            #expect(gate.remainingSeconds(view, at: SettingsInstant.reference) == 0)
            #expect(gate.expiresAt(view) == nil)
        }
        #expect(gate.graceWindowSeconds == 300)
        #expect(LocalAuthGate.graceWindowSeconds == 300)
    }

    @Test("unlocking one view leaves the other locked")
    func grantsDoNotLeakBetweenViews() {
        let gate = LocalAuthGate()

        gate.grant(.hidden, at: SettingsInstant.reference)

        #expect(gate.isUnlocked(.hidden, at: SettingsInstant.reference))
        #expect(!gate.isUnlocked(.recentlyDeleted, at: SettingsInstant.reference))
        #expect(gate.remainingSeconds(.recentlyDeleted, at: SettingsInstant.reference) == 0)
    }

    /// A grant is spent exactly at the window's end, so five minutes grants for
    /// 300 seconds and not 301.
    @Test(
        "the window boundary is exclusive",
        arguments: [
            GraceSample(elapsed: 0, remaining: 300, unlocked: true),
            GraceSample(elapsed: 1, remaining: 299, unlocked: true),
            GraceSample(elapsed: 299, remaining: 1, unlocked: true),
            GraceSample(elapsed: 300, remaining: 0, unlocked: false),
            GraceSample(elapsed: 301, remaining: 0, unlocked: false),
            GraceSample(elapsed: 100000, remaining: 0, unlocked: false),
        ]
    )
    func windowBoundaryIsExclusive(sample: GraceSample) {
        let gate = LocalAuthGate()
        gate.grant(.hidden, at: SettingsInstant.reference)

        let now = SettingsInstant.seconds(sample.elapsed)

        #expect(gate.remainingSeconds(.hidden, at: now) == sample.remaining)
        #expect(gate.isUnlocked(.hidden, at: now) == sample.unlocked)
    }

    @Test("a deployment that shortened the window is not describing five minutes")
    func configuredWindowIsWhatIsEnforced() {
        let gate = LocalAuthGate(windowSeconds: 60)
        gate.grant(.hidden, at: SettingsInstant.reference)

        #expect(gate.graceWindowSeconds == 60)
        #expect(gate.isUnlocked(.hidden, at: SettingsInstant.seconds(59)))
        #expect(!gate.isUnlocked(.hidden, at: SettingsInstant.seconds(60)))
        #expect(gate.expiresAt(.hidden) == SettingsInstant.seconds(60))
    }

    @Test("a clock that moved backwards does not silently extend or void a grant")
    func backwardsClockIsHandled() {
        let gate = LocalAuthGate()
        gate.grant(.hidden, at: SettingsInstant.reference)

        let remaining = gate.remainingSeconds(.hidden, at: SettingsInstant.seconds(-500))

        #expect(remaining == 300)
        #expect(gate.isUnlocked(.hidden, at: SettingsInstant.seconds(-500)))
    }

    @Test("locking one view drops only that grant; locking all drops every one")
    func revocationIsPerViewAndWholesale() {
        let gate = LocalAuthGate()
        gate.grant(.hidden, at: SettingsInstant.reference)
        gate.grant(.recentlyDeleted, at: SettingsInstant.reference)

        gate.revoke(.hidden)

        #expect(!gate.isUnlocked(.hidden, at: SettingsInstant.reference))
        #expect(gate.isUnlocked(.recentlyDeleted, at: SettingsInstant.reference))

        gate.revokeAll()

        #expect(!gate.isUnlocked(.recentlyDeleted, at: SettingsInstant.reference))
        #expect(gate.expiresAt(.recentlyDeleted) == nil)
    }

    @Test("re-granting restarts the window from the new instant")
    func regrantRestartsTheWindow() {
        let gate = LocalAuthGate()
        gate.grant(.hidden, at: SettingsInstant.reference)

        gate.grant(.hidden, at: SettingsInstant.seconds(250))

        #expect(gate.remainingSeconds(.hidden, at: SettingsInstant.seconds(250)) == 300)
        #expect(gate.isUnlocked(.hidden, at: SettingsInstant.seconds(500)))
    }

    @Test("the method a device would use is reported, unavailability included")
    func methodsAreReportedHonestly() {
        #expect(LocalAuthMethod.biometric.tone == .positive)
        #expect(LocalAuthMethod.deviceCredential.tone == .positive)
        #expect(LocalAuthMethod.unavailable.tone == .caution, "an unprotected trash is not good news")

        let keys = [LocalAuthMethod.biometric, .deviceCredential, .unavailable].map(\.descriptionKey)
        #expect(Set(keys).count == 3)
        for key in keys {
            #expect(key.hasPrefix("ios.settings.security.method."))
        }
    }
}

// MARK: - SecuritySettingsTests

/// The screen over the gate.
@Suite("A gated view stays gated unless the challenge actually succeeded")
@MainActor
struct SecuritySettingsTests {
    private static func model(
        authenticator: StubLocalAuthenticator,
        clock: SettingsClock = SettingsInstant.clock,
        gate: LocalAuthGate = LocalAuthGate()
    ) -> SecuritySettingsModel {
        SecuritySettingsModel(
            auth: StubAuthPort(),
            authenticator: authenticator,
            connectivity: .stub(),
            clock: clock,
            gate: gate,
            posture: .sandboxPrivate
        )
    }

    @Test("loading reports what this device would actually challenge with")
    func loadReportsTheMethod() async {
        let model = Self.model(authenticator: StubLocalAuthenticator(method: .deviceCredential))

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.method == .deviceCredential)
        #expect(model.gatedViews == GatedLibraryView.allCases)
        #expect(model.graceWindowSeconds == 300)
    }

    @Test("a successful challenge opens only the view that was asked for")
    func successUnlocksOneView() async {
        let model = Self.model(authenticator: StubLocalAuthenticator())
        await model.load()

        await model.unlock(.hidden)

        #expect(model.isUnlocked(.hidden))
        #expect(!model.isUnlocked(.recentlyDeleted), "granting both would halve the protection")
        #expect(model.remainingSeconds(.hidden) == 300)
        #expect(!model.lastChallengeCancelled)
    }

    /// A cancel is not an error and must not be reported as one — but it is
    /// certainly not a success either.
    @Test("a cancelled prompt leaves the view locked and is not reported as a failure")
    func cancelledPromptIsNotSuccess() async {
        let model = Self.model(authenticator: StubLocalAuthenticator(outcome: .cancelled))
        await model.load()

        await model.unlock(.hidden)

        #expect(!model.isUnlocked(.hidden))
        #expect(model.lastChallengeCancelled)
        #expect(model.phase == .ready, "a dismissed sheet is not an error state")
        #expect(model.remainingSeconds(.hidden) == 0)
    }

    @Test("a failed challenge leaves the view locked and surfaces the failure")
    func failedChallengeLeavesTheViewLocked() async {
        let failing = StubLocalAuthenticator(outcome: .failed(StubError.failure(.syncUnauthenticated)))
        let model = Self.model(authenticator: failing)
        await model.load()

        await model.unlock(.recentlyDeleted)

        #expect(!model.isUnlocked(.recentlyDeleted))
        #expect(model.phase == .failed(.syncUnauthenticated))
        #expect(!model.lastChallengeCancelled, "a thrown ceremony is not a user cancelling")
    }

    @Test("a cancelled prompt followed by a granted one clears the cancelled note")
    func cancellationIsClearedOnTheNextTry() async {
        let gate = LocalAuthGate()
        let cancelled = Self.model(authenticator: StubLocalAuthenticator(outcome: .cancelled), gate: gate)
        await cancelled.unlock(.hidden)
        #expect(cancelled.lastChallengeCancelled)

        let granted = Self.model(authenticator: StubLocalAuthenticator(), gate: gate)
        await granted.unlock(.hidden)

        #expect(!granted.lastChallengeCancelled)
        #expect(granted.isUnlocked(.hidden))
    }

    @Test("the grant expires against the injected clock rather than lasting the session")
    func grantExpiresOnTheClock() async {
        let clock = MovableClock()
        let model = Self.model(authenticator: StubLocalAuthenticator(), clock: clock.settingsClock)
        await model.load()
        await model.unlock(.hidden)
        #expect(model.isUnlocked(.hidden))

        clock.advance(seconds: 299)
        #expect(model.isUnlocked(.hidden))
        #expect(model.remainingSeconds(.hidden) == 1)

        clock.advance(seconds: 1)

        #expect(!model.isUnlocked(.hidden), "the window closes at 300 seconds, not at the end of the session")
        #expect(model.remainingSeconds(.hidden) == 0)
    }

    @Test("locking now drops the grant before the window would have closed")
    func lockingNowIsImmediate() async {
        let model = Self.model(authenticator: StubLocalAuthenticator())
        await model.unlock(.hidden)
        await model.unlock(.recentlyDeleted)

        model.lock(.hidden)
        #expect(!model.isUnlocked(.hidden))
        #expect(model.isUnlocked(.recentlyDeleted))

        model.lockAll()
        #expect(!model.isUnlocked(.recentlyDeleted))
    }

    @Test("the screen carries the platform's own at-rest row")
    func postureIsCarried() {
        let model = Self.model(authenticator: StubLocalAuthenticator())

        #expect(model.posture == .sandboxPrivate)
    }
}
