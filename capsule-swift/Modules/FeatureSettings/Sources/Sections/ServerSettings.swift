import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - ServerSettingsModel

/// Drives the Server screen: which home server this account lives on, whether
/// the device can currently reach it, and what version the two of them agree on.
///
/// The protocol version is on this screen rather than only in Advanced because
/// it is the thing that decides whether a write will be accepted at all: "a
/// client whose `protocol_version` falls below an album's pin is rejected for
/// writes to *that album* … Reads are unaffected." A user who cannot save an
/// edit is owed that sentence somewhere they would look.
@MainActor
@Observable
public final class ServerSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var account: AccountSummary?
    public private(set) var status: SyncStatus?
    public private(set) var isWorking = false

    private let auth: any AuthPort
    private let sync: any SyncPort
    private let connectivity: SettingsConnectivity
    public let buildInfo: SettingsBuildInfo

    public init(
        auth: any AuthPort,
        sync: any SyncPort,
        connectivity: SettingsConnectivity,
        buildInfo: SettingsBuildInfo
    ) {
        self.auth = auth
        self.sync = sync
        self.connectivity = connectivity
        self.buildInfo = buildInfo
    }

    public func load() async {
        phase = .loading
        account = await auth.state().account
        do {
            status = try await sync.status()
            phase = account == nil ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// The connection class, and how loudly to report it.
    public var connectionStatus: (key: String, tone: SettingsTone) {
        guard let connection = status?.connectionClass else {
            return ("app.settings.server.connection.unknown", .neutral)
        }
        return (ConnectionClassPresentation.titleKey(connection), connection.tone)
    }

    /// Try a reconciliation now, subject to the connection criteria.
    public func synchronize() async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await sync.synchronize()
            status = try await sync.status()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

private extension AuthState {
    var account: AccountSummary? {
        switch self {
        case .signedOut: nil
        case let .signedIn(summary), let .requiresLocalAuth(summary), let .expired(summary):
            summary
        }
    }
}

// MARK: - ServerSettingsView

/// Server — home server, reachability, and version agreement.
public struct ServerSettingsView: View {
    @State private var model: ServerSettingsModel

    public init(model: ServerSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: ServerSettingsModel(
                auth: environment.auth,
                sync: environment.sync,
                connectivity: environment.connectivity,
                buildInfo: environment.buildInfo
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.server.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.server.empty.title",
            emptyDescriptionKey: "app.settings.server.empty.description",
            retry: { await model.load() },
            content: {
                originSection
                versionSection
                portabilitySection
            }
        )
        .task { await model.load() }
    }

    private var originSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.server.origin",
                value: model.account?.homeServer ?? SettingsFormat.unknown
            )
            SettingsStatusRow(
                labelKey: "app.settings.server.connection.label",
                statusKey: model.connectionStatus.key,
                tone: model.connectionStatus.tone
            )
            SettingsValueRow(
                labelKey: "app.settings.server.last_sync",
                value: SettingsFormat.timestamp(model.status?.lastCompletedSyncAt)
            )
            Button("app.settings.server.sync_now") {
                Task { await model.synchronize() }
            }
            .disabled(model.isWorking)
        } header: {
            Text("app.settings.server.origin.header")
        } footer: {
            Text("app.settings.server.origin.footer")
        }
    }

    private var versionSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.server.protocol_version",
                value: model.buildInfo.protocolVersion
            )
            SettingsValueRow(
                labelKey: "app.settings.server.client_version",
                value: model.buildInfo.clientVersion
            )
        } header: {
            Text("app.settings.server.version.header")
        } footer: {
            Text("app.settings.server.version.footer")
        }
    }

    private var portabilitySection: some View {
        Section {
            SettingsNoteRow(textKey: "app.settings.server.portability.body")
        } header: {
            Text("app.settings.server.portability.header")
        } footer: {
            Text("app.settings.server.portability.footer")
        }
    }
}

// MARK: - Preview

#Preview("Server") {
    NavigationStack {
        ServerSettingsView(environment: .preview())
    }
}

#Preview("Server — Offline") {
    NavigationStack {
        ServerSettingsView(environment: .preview(.offline))
    }
}
