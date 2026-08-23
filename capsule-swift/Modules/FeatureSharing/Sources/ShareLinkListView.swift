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
                title: "app.share.list.empty.title",
                message: "app.share.list.empty.description",
                symbol: "link"
            ),
            retry: { Task { await model.load() } },
            content: {
                list
            }
        )
        .navigationTitle("app.share.list.title")
        .task { await model.load() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .confirmationDialog(
            "app.share.list.revoke.confirm_title",
            isPresented: Binding(
                get: { model.pendingRevocation != nil },
                set: { if !$0 { model.pendingRevocation = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("app.share.list.revoke.confirm", role: .destructive) {
                guard let row = model.pendingRevocation else { return }
                Task { await model.revoke(row) }
            }
            Button("app.common.cancel", role: .cancel) { model.pendingRevocation = nil }
        } message: {
            Text("app.share.list.revoke.confirm_message")
        }
    }

    private var list: some View {
        List {
            if !model.active.isEmpty {
                Section("app.share.list.section.active") {
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
                    Text("app.share.list.section.record")
                } footer: {
                    Text("app.share.list.section.record_footer")
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
                Button("app.share.list.revoke", role: .destructive, action: revoke)
            }
        }
        .contextMenu {
            if let revoke {
                Button("app.share.list.revoke", role: .destructive, action: revoke)
            }
        }
    }

    @ViewBuilder
    private var detailRows: some View {
        if let expiresAt = row.expiresAt {
            timestampRow("app.share.list.expires", expiresAt)
        }
        if let createdAt = row.createdAt {
            timestampRow("app.share.list.created", createdAt)
        } else {
            unavailableRow("app.share.list.created", "app.share.list.created.unknown")
        }
        if let lastUsedAt = row.lastUsedAt {
            timestampRow("app.share.list.last_used", lastUsedAt)
        } else {
            unavailableRow("app.share.list.last_used", "app.share.list.last_used.unrecorded")
        }
        if row.hasPassphrase {
            StatusBadge(title: "app.share.list.passphrase", symbol: "key", tint: .secondary)
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
        case .revoked: StatusBadge(title: "app.share.list.status.revoked", symbol: "xmark.seal", tint: .red)
        case .expired: StatusBadge(title: "app.share.list.status.expired", symbol: "clock.badge.xmark", tint: .orange)
        case nil: StatusBadge(title: "app.share.list.status.live", symbol: "checkmark.seal", tint: .green)
        }
    }

    private var scopeTitle: LocalizedStringKey {
        row.scope.isAlbumWide ? "app.share.scope.album" : "app.share.scope.asset"
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
