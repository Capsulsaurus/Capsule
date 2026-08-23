import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - AccountSettingsModel

/// Drives the Account screen: who is signed in, on what, and how to stop being.
///
/// No token reaches this model, and none can: ``AuthState`` carries an
/// ``AccountSummary`` and nothing else. That is the property the port was shaped
/// around — a credential that reached a view model is a credential that can
/// reach a log, a crash report, or a screenshot.
@MainActor
@Observable
public final class AccountSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var state: AuthState = .signedOut
    public private(set) var sessions: [SessionRecord] = []
    public private(set) var isWorking = false

    private let auth: any AuthPort
    private let devices: any DevicePort
    private let connectivity: SettingsConnectivity
    private let clock: SettingsClock

    public init(
        auth: any AuthPort,
        devices: any DevicePort,
        connectivity: SettingsConnectivity,
        clock: SettingsClock = .system
    ) {
        self.auth = auth
        self.devices = devices
        self.connectivity = connectivity
        self.clock = clock
    }

    public func load() async {
        phase = .loading
        state = await auth.state()
        guard account != nil else {
            phase = .empty
            return
        }
        do {
            sessions = try await devices.sessions()
            phase = .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// The signed-in account, whatever flavour of session state it is in.
    public var account: AccountSummary? {
        switch state {
        case .signedOut: nil
        case let .signedIn(summary): summary
        case let .requiresLocalAuth(summary): summary
        case let .expired(summary): summary
        }
    }

    /// The catalog key for the session's standing, and how loudly to say it.
    public var sessionStatus: (key: String, tone: SettingsTone) {
        switch state {
        case .signedOut: ("app.settings.account.state.signed_out", .caution)
        case .signedIn: ("app.settings.account.state.signed_in", .positive)
        case .requiresLocalAuth: ("app.settings.account.state.requires_local_auth", .caution)
        case .expired: ("app.settings.account.state.expired", .critical)
        }
    }

    /// Sessions still honoured right now, newest first.
    public var liveSessions: [SessionRecord] {
        sessions.filter { $0.isLive(at: clock.now()) }
            .sorted { $0.lastUsedAt > $1.lastUsedAt }
    }

    /// The session this app is using, when the ledger names it.
    public var currentSession: SessionRecord? {
        sessions.first(where: \.isCurrent)
    }

    public func confirmLocalAuthentication() async {
        await perform {
            try await self.auth.confirmLocalAuthentication()
            self.state = await self.auth.state()
        }
    }

    public func signOut() async {
        await perform {
            try await self.auth.signOut()
            self.state = await self.auth.state()
            self.sessions = []
        }
    }

    /// Revoke one session — authenticated by any active session token.
    public func revoke(_ id: SessionID) async {
        await perform {
            try await self.devices.revokeSession(id)
            self.sessions = try await self.devices.sessions()
        }
    }

    /// Revoke every session.
    ///
    /// Authenticated by **proof of master-key possession**, not by a session
    /// token — deliberately asymmetric, so a stolen token can revoke only
    /// itself. The ceremony is driven inside the port; a request without valid
    /// proof revokes nothing at all, so there is no partial state to repair.
    public func revokeAllSessions() async {
        await perform {
            try await self.devices.revokeAllSessions()
            self.sessions = try await self.devices.sessions()
        }
    }

    private func perform(_ work: @escaping () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await work()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

// MARK: - AccountSettingsView

/// Account — identity, session standing, and the session ledger.
public struct AccountSettingsView: View {
    @State private var model: AccountSettingsModel
    @State private var isSignOutPresented = false
    @State private var isRevokeAllPresented = false

    public init(model: AccountSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: AccountSettingsModel(
                auth: environment.auth,
                devices: environment.devices,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.account.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.account.empty.title",
            emptyDescriptionKey: "app.settings.account.empty.description",
            retry: { await model.load() },
            content: {
                identitySection
                sessionsSection
                actionsSection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.account.signout.confirm.title",
            messageKey: "app.settings.account.signout.confirm.message",
            confirmKey: "app.settings.account.signout.confirm.action",
            isPresented: $isSignOutPresented
        ) {
            await model.signOut()
        }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.account.revoke_all.confirm.title",
            messageKey: "app.settings.account.revoke_all.confirm.message",
            confirmKey: "app.settings.account.revoke_all.confirm.action",
            isPresented: $isRevokeAllPresented
        ) {
            await model.revokeAllSessions()
        }
    }

    private var identitySection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.account.handle",
                value: model.account?.handle ?? SettingsFormat.unknown
            )
            SettingsValueRow(
                labelKey: "app.settings.account.display_name",
                value: model.account?.displayName ?? SettingsFormat.unknown
            )
            SettingsValueRow(
                labelKey: "app.settings.account.home_server",
                value: model.account?.homeServer ?? SettingsFormat.unknown
            )
            SettingsStatusRow(
                labelKey: "app.settings.account.state.label",
                statusKey: model.sessionStatus.key,
                tone: model.sessionStatus.tone
            )
            accountTypeRow
        } header: {
            Text("app.settings.account.identity.header")
        } footer: {
            Text("app.settings.account.identity.footer")
        }
    }

    @ViewBuilder
    private var accountTypeRow: some View {
        if let type = model.account?.accountType {
            SettingsStatusRow(
                labelKey: "app.settings.account.type.label",
                statusKey: type == .sponsored
                    ? "app.settings.account.type.sponsored"
                    : "app.settings.account.type.registered",
                tone: .neutral
            )
        }
    }

    private var sessionsSection: some View {
        Section {
            if model.liveSessions.isEmpty {
                SettingsNoteRow(textKey: "app.settings.account.sessions.none")
            }
            ForEach(model.liveSessions) { session in
                sessionRows(session)
            }
        } header: {
            Text("app.settings.account.sessions.header")
        } footer: {
            Text("app.settings.account.sessions.footer")
        }
    }

    @ViewBuilder
    private func sessionRows(_ session: SessionRecord) -> some View {
        SettingsStatusRow(
            labelKey: "app.settings.account.sessions.session",
            statusKey: session.isCurrent
                ? "app.settings.account.sessions.current"
                : "app.settings.account.sessions.other",
            tone: session.isCurrent ? .positive : .neutral
        )
        SettingsValueRow(
            labelKey: "app.settings.account.sessions.last_used",
            value: SettingsFormat.timestamp(session.lastUsedAt)
        )
        SettingsValueRow(
            labelKey: "app.settings.account.sessions.expires",
            value: SettingsFormat.day(session.effectiveExpiry)
        )
        if !session.isCurrent {
            Button("app.settings.account.sessions.revoke", role: .destructive) {
                Task { await model.revoke(session.id) }
            }
        }
    }

    private var actionsSection: some View {
        Section {
            if case .requiresLocalAuth = model.state {
                Button("app.settings.account.confirm_local_auth") {
                    Task { await model.confirmLocalAuthentication() }
                }
            }
            Button("app.settings.account.revoke_all", role: .destructive) {
                isRevokeAllPresented = true
            }
            .disabled(model.isWorking)
            Button("app.settings.account.signout", role: .destructive) {
                isSignOutPresented = true
            }
            .disabled(model.isWorking)
            SettingsNoteRow(textKey: "app.settings.account.portability.body")
        } header: {
            Text("app.settings.account.actions.header")
        } footer: {
            Text("app.settings.account.actions.footer")
        }
    }
}

// MARK: - Preview

#Preview("Account") {
    NavigationStack {
        AccountSettingsView(environment: .preview())
    }
}

#Preview("Account — Signed Out") {
    NavigationStack {
        AccountSettingsView(environment: .preview(.neverSignedIn))
    }
}
