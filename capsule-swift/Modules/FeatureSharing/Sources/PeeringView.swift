import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - PeeringView

/// Nearby devices of this user's own, and what is moving between them
/// (*Peering*).
///
/// Note what is absent: no retry button on an empty list, no red badge, no
/// "peering failed". Peering is an accelerator; when it finds nothing the device
/// syncs through the server and nobody needs to be told.
public struct PeeringView: View {
    @State private var model: PeeringViewModel

    public init(model: PeeringViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        Form {
            enableSection
            if model.isEnabled {
                if model.isIdleWithNoPeers {
                    idleSection
                } else {
                    peerSections
                }
                transferSection
            }
            scopeSection
        }
        .formStyle(.grouped)
        .frame(maxWidth: 720)
        .frame(maxWidth: .infinity)
        .navigationTitle("ios.peering.title")
        .task { await model.load() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
    }

    // MARK: Sections

    private var enableSection: some View {
        Section {
            Toggle("ios.peering.enable", isOn: Binding(
                get: { model.isEnabled },
                set: { enabled in Task { await model.setEnabled(enabled) } }
            ))
        } footer: {
            Text("ios.peering.enable.footer")
        }
    }

    /// Peering is on and the network is quiet. Reassuring, not diagnostic.
    private var idleSection: some View {
        Section {
            ContentUnavailableView(
                "ios.peering.idle.title",
                systemImage: "wifi",
                description: Text("ios.peering.idle.description")
            )
        }
    }

    @ViewBuilder
    private var peerSections: some View {
        if !model.pairedPeers.isEmpty {
            Section("ios.peering.section.paired") {
                ForEach(model.pairedPeers) { PeerRowView(peer: $0) }
            }
        }
        if !model.unpairedPeers.isEmpty {
            Section {
                ForEach(model.unpairedPeers) { PeerRowView(peer: $0) }
            } header: {
                Text("ios.peering.section.unpaired")
            } footer: {
                Text("ios.peering.section.unpaired_footer")
            }
        }
    }

    @ViewBuilder
    private var transferSection: some View {
        if model.transfers.isEmpty {
            Section("ios.peering.section.transfers") {
                Text("ios.peering.transfers.none")
                    .foregroundStyle(.secondary)
            }
        } else {
            Section("ios.peering.section.transfers") {
                ForEach(model.transfers) { TransferRowView(transfer: $0) }
            }
        }
    }

    /// The rules a user would otherwise have to guess at: LAN only, pull-only,
    /// and no peering-specific sync scope — peering honours the library's
    /// existing scope and there is deliberately no second knob.
    private var scopeSection: some View {
        Section("ios.peering.section.how") {
            ScopeNote(message: "ios.peering.note.lan_only")
            ScopeNote(message: "ios.peering.note.pull_only")
            ScopeNote(message: "ios.peering.note.same_account")
            ScopeNote(message: "ios.peering.note.no_scope_knob")
        }
    }
}

// MARK: - PeerRowView

/// One discovered device.
struct PeerRowView: View {
    let peer: LocalPeer

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            HStack {
                Label {
                    Text(verbatim: peer.model)
                } icon: {
                    Image(systemName: symbol)
                }
                Spacer(minLength: CapsuleTheme.Spacing.small)
                trustBadge
            }
            LabeledContent {
                Text(peer.lastSeenAt.date, format: .relative(presentation: .named))
            } label: {
                Text("ios.peering.peer.last_seen")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var trustBadge: some View {
        switch peer.trust {
        case .paired:
            StatusBadge(title: "ios.peering.trust.paired", symbol: "checkmark.shield", tint: .green)
        case .discovered:
            StatusBadge(title: "ios.peering.trust.discovered", symbol: "questionmark.circle", tint: .secondary)
        case .revoked:
            StatusBadge(title: "ios.peering.trust.revoked", symbol: "xmark.shield", tint: .red)
        }
    }

    private var symbol: String {
        switch peer.platform {
        case .ios: "iphone"
        case .macos: "laptopcomputer"
        case .android: "candybarphone"
        case .windows, .linux: "desktopcomputer"
        case .unknown: "questionmark.square.dashed"
        }
    }
}

// MARK: - TransferRowView

/// One in-flight LAN transfer.
///
/// Transfers are resumable and content-addressed, so a resumed transfer can jump
/// forward without any bytes moving. The progress bar shows the fraction of the
/// blob in hand, not the fraction of this session's work.
struct TransferRowView: View {
    let transfer: PeeringTransfer

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            HStack {
                Label(directionTitle, systemImage: directionSymbol)
                    .font(.subheadline)
                Spacer(minLength: CapsuleTheme.Spacing.small)
                Text(Int64(transfer.totalBytes), format: .byteCount(style: .file))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ProgressView(value: transfer.fractionComplete)
                .accessibilityLabel(directionTitle)
                .accessibilityValue(Text(transfer.fractionComplete, format: .percent.precision(.fractionLength(0))))
        }
        .accessibilityElement(children: .combine)
    }

    private var directionTitle: LocalizedStringKey {
        transfer.direction == .receiving ? "ios.peering.transfer.receiving" : "ios.peering.transfer.sending"
    }

    private var directionSymbol: String {
        transfer.direction == .receiving ? "arrow.down.circle" : "arrow.up.circle"
    }
}

// MARK: - Previews

#Preview("Peering — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        PeeringView(model: PeeringViewModel(
            peering: environment.peering,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Peering — dark, offline") {
    let environment = MockEnvironment(scenario: .offline)
    return NavigationStack {
        PeeringView(model: PeeringViewModel(
            peering: environment.peering,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.dark)
}
