import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - SecuritySettingsModel

/// Drives the Security & Privacy screen.
///
/// Two facts this screen exists to state honestly, both from *Local Gallery*:
///
/// - The Recently Deleted and Hidden gate is "view-time UX protection against a
///   borrowed-unlocked-phone snoop; it is not a cryptographic boundary". The
///   copy says that outright rather than implying encryption.
/// - The at-rest posture is **the running platform's row of the SR2 table**, not
///   a generic reassurance. On macOS that row says any process running as the
///   user can read the library, and this screen says so.
@MainActor
@Observable
public final class SecuritySettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var method: LocalAuthMethod = .deviceCredential
    /// Set when the last challenge was dismissed by the user. A cancel is not a
    /// failure, so it is reported as its own state and cleared on the next try.
    public private(set) var lastChallengeCancelled = false

    /// The per-view grace windows. Public so the app can share one gate with
    /// the library screens that actually open behind it.
    public let gate: LocalAuthGate
    /// The SR2 row for this platform.
    public let posture: AtRestPosture

    private let auth: any AuthPort
    private let authenticator: any LocalAuthenticator
    private let connectivity: SettingsConnectivity
    private let clock: SettingsClock

    public init(
        auth: any AuthPort,
        authenticator: any LocalAuthenticator = SystemLocalAuthenticator(),
        connectivity: SettingsConnectivity,
        clock: SettingsClock = .system,
        gate: LocalAuthGate = LocalAuthGate(),
        posture: AtRestPosture = .current
    ) {
        self.auth = auth
        self.authenticator = authenticator
        self.connectivity = connectivity
        self.clock = clock
        self.gate = gate
        self.posture = posture
    }

    /// The configured window, in seconds.
    public var graceWindowSeconds: Int64 { gate.graceWindowSeconds }

    /// Every gated view, in a stable order.
    public var gatedViews: [GatedLibraryView] { GatedLibraryView.allCases }

    public func load() async {
        phase = .loading
        method = await authenticator.availableMethod()
        _ = await auth.state()
        phase = .ready
    }

    /// Whether a view would open without a fresh challenge, right now.
    public func isUnlocked(_ view: GatedLibraryView) -> Bool {
        gate.isUnlocked(view, at: clock.now())
    }

    /// Grace remaining on one view, in seconds.
    public func remainingSeconds(_ view: GatedLibraryView) -> Int64 {
        gate.remainingSeconds(view, at: clock.now())
    }

    /// Run the challenge for one view and, on success, start its own window.
    ///
    /// Deliberately grants **only** the view asked for: SR1's window is
    /// per-view, and granting both here would quietly halve the protection the
    /// second view is supposed to have.
    public func unlock(_ view: GatedLibraryView) async {
        lastChallengeCancelled = false
        do {
            let granted = try await authenticator.authenticate(
                reasonKey: "app.settings.security.gate.reason"
            )
            if granted {
                gate.grant(view, at: clock.now())
            } else {
                lastChallengeCancelled = true
            }
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    public func lock(_ view: GatedLibraryView) {
        gate.revoke(view)
    }

    public func lockAll() {
        gate.revokeAll()
    }
}

// MARK: - SecuritySettingsView

/// Security & Privacy.
public struct SecuritySettingsView: View {
    @State private var model: SecuritySettingsModel

    public init(model: SecuritySettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: SecuritySettingsModel(
                auth: environment.auth,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.security.titleKey,
            phase: model.phase,
            retry: { await model.load() },
            content: {
                gateSection
                methodSection
                atRestSection
                plaintextSection
            }
        )
        .task { await model.load() }
    }

    private var gateSection: some View {
        Section {
            ForEach(model.gatedViews, id: \.self) { view in
                gateRow(view)
            }
            SettingsValueRow(
                labelKey: "app.settings.security.gate.window",
                value: SettingsFormat.minutes(seconds: model.graceWindowSeconds)
            )
            Button("app.settings.security.gate.lock_all") { model.lockAll() }
                .accessibilityLabel(Text("app.settings.security.gate.lock_all"))
        } header: {
            Text("app.settings.security.gate.header")
        } footer: {
            Text("app.settings.security.gate.footer")
        }
    }

    @ViewBuilder
    private func gateRow(_ view: GatedLibraryView) -> some View {
        let unlocked = model.isUnlocked(view)
        SettingsStatusRow(
            labelKey: view.titleKey,
            statusKey: unlocked
                ? "app.settings.security.gate.state.unlocked"
                : "app.settings.security.gate.state.locked",
            tone: unlocked ? .caution : .positive
        )
        if unlocked {
            SettingsValueRow(
                labelKey: "app.settings.security.gate.remaining",
                value: SettingsFormat.duration(seconds: model.remainingSeconds(view))
            )
            Button("app.settings.security.gate.lock") { model.lock(view) }
        } else {
            Button("app.settings.security.gate.unlock") {
                Task { await model.unlock(view) }
            }
        }
    }

    private var methodSection: some View {
        Section {
            SettingsStatusRow(
                labelKey: "app.settings.security.method.label",
                statusKey: model.method.descriptionKey,
                tone: model.method.tone
            )
            if model.lastChallengeCancelled {
                SettingsNoteRow(textKey: "app.settings.security.gate.cancelled")
            }
            SettingsStatusRow(
                labelKey: "app.settings.security.capture.label",
                statusKey: "app.settings.security.capture.status",
                tone: .neutral
            )
        } header: {
            Text("app.settings.security.method.header")
        } footer: {
            Text("app.settings.security.method.footer")
        }
    }

    private var atRestSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.security.atrest.store.label",
                value: SettingsPhrase.text(forKey: model.posture.storeKey)
            )
            SettingsStatusRow(
                labelKey: "app.settings.security.atrest.protection.label",
                statusKey: model.posture.protectionKey,
                tone: model.posture.tone
            )
        } header: {
            Text("app.settings.security.atrest.header")
        } footer: {
            Text(LocalizedStringKey(model.posture.summaryKey))
        }
    }

    private var plaintextSection: some View {
        Section {
            SettingsNoteRow(textKey: "app.settings.security.plaintext.body")
        } header: {
            Text("app.settings.security.plaintext.header")
        } footer: {
            Text("app.settings.security.plaintext.footer")
        }
    }
}

// MARK: - Preview

#Preview("Security") {
    NavigationStack {
        SecuritySettingsView(environment: .preview())
    }
}

#Preview("Security — Dark") {
    NavigationStack {
        SecuritySettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
