import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ShareLinkListView

/// Every link this user has issued, live ones first (*Share Links*).
///
/// Revoked and expired links keep their rows. The list is a record of what was
/// handed out, and a link already opened cannot be un-shared — so a disappearing
/// row would tell a comforting lie.
public struct ShareLinkListView: View {
    @State private var model: ShareLinkListViewModel

    public init(model: ShareLinkListViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        SharingStateView(
            phase: model.phase,
            empty: .init(
                title: "ios.share.list.empty.title",
                message: "ios.share.list.empty.description",
                symbol: "link"
            ),
            retry: { Task { await model.load() } },
            content: {
                list
            }
        )
        .navigationTitle("ios.share.list.title")
        .task { await model.load() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .confirmationDialog(
            "ios.share.list.revoke.confirm_title",
            isPresented: Binding(
                get: { model.pendingRevocation != nil },
                set: { if !$0 { model.pendingRevocation = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("ios.share.list.revoke.confirm", role: .destructive) {
                guard let row = model.pendingRevocation else { return }
                Task { await model.revoke(row) }
            }
            Button("ios.common.cancel", role: .cancel) { model.pendingRevocation = nil }
        } message: {
            Text("ios.share.list.revoke.confirm_message")
        }
    }

    private var list: some View {
        List {
            if !model.active.isEmpty {
                Section("ios.share.list.section.active") {
                    ForEach(model.active) { row in
                        ShareLinkRowView(row: row) { model.pendingRevocation = row }
                    }
                }
            }
            if !model.inactive.isEmpty {
                Section {
                    ForEach(model.inactive) { row in
                        ShareLinkRowView(row: row, revoke: nil)
                    }
                } header: {
                    Text("ios.share.list.section.record")
                } footer: {
                    Text("ios.share.list.section.record_footer")
                }
            }
        }
    }
}

// MARK: - ShareLinkRowView

/// One issued link.
///
/// The URL is **not** here. A list row is copied into screenshots, read out by
/// VoiceOver, and captured by every crash reporter that snapshots a view
/// hierarchy; the fragment secret belongs on exactly one screen, at the moment
/// of copying.
struct ShareLinkRowView: View {
    let row: ShareLinkRow
    let revoke: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            HStack {
                Label(scopeTitle, systemImage: row.scope.isAlbumWide ? "rectangle.stack" : "photo")
                Spacer(minLength: CapsuleTheme.Spacing.small)
                statusBadge
            }
            detailRows
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
        .swipeActions(edge: .trailing) {
            if let revoke {
                Button("ios.share.list.revoke", role: .destructive, action: revoke)
            }
        }
        .contextMenu {
            if let revoke {
                Button("ios.share.list.revoke", role: .destructive, action: revoke)
            }
        }
    }

    @ViewBuilder
    private var detailRows: some View {
        if let expiresAt = row.expiresAt {
            timestampRow("ios.share.list.expires", expiresAt)
        }
        if let createdAt = row.createdAt {
            timestampRow("ios.share.list.created", createdAt)
        } else {
            unavailableRow("ios.share.list.created", "ios.share.list.created.unknown")
        }
        if let lastUsedAt = row.lastUsedAt {
            timestampRow("ios.share.list.last_used", lastUsedAt)
        } else {
            unavailableRow("ios.share.list.last_used", "ios.share.list.last_used.unrecorded")
        }
        if row.hasPassphrase {
            StatusBadge(title: "ios.share.list.passphrase", symbol: "key", tint: .secondary)
        }
    }

    private func timestampRow(_ label: LocalizedStringKey, _ instant: CapsuleTimestamp) -> some View {
        LabeledContent {
            Text(instant.date, format: .dateTime.year().month().day().hour().minute())
        } label: {
            Text(label)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    private func unavailableRow(_ label: LocalizedStringKey, _ value: LocalizedStringKey) -> some View {
        LabeledContent {
            Text(value)
        } label: {
            Text(label)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch row.lapse {
        case .revoked: StatusBadge(title: "ios.share.list.status.revoked", symbol: "xmark.seal", tint: .red)
        case .expired: StatusBadge(title: "ios.share.list.status.expired", symbol: "clock.badge.xmark", tint: .orange)
        case nil: StatusBadge(title: "ios.share.list.status.live", symbol: "checkmark.seal", tint: .green)
        }
    }

    private var scopeTitle: LocalizedStringKey {
        row.scope.isAlbumWide ? "ios.share.scope.album" : "ios.share.scope.asset"
    }
}

// MARK: - Previews

#Preview("Share links — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        ShareLinkListView(model: ShareLinkListViewModel(
            share: environment.sharing,
            connectivity: SharingConnectivity(sync: environment.sync),
            now: { MockClock.reference.now }
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Share links — dark") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        ShareLinkListView(model: ShareLinkListViewModel(
            share: environment.sharing,
            connectivity: SharingConnectivity(sync: environment.sync),
            now: { MockClock.reference.now }
        ))
    }
    .preferredColorScheme(.dark)
}
