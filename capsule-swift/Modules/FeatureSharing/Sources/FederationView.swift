import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - FederationView

/// Aggregated albums, their constituents, and the peer servers behind them
/// (*Federation*).
///
/// Two-column at regular width so an album's per-origin availability sits beside
/// the list rather than a push away — "why is half of this album missing" is a
/// question best answered without navigating.
public struct FederationView: View {
    @State private var model: FederationViewModel

    public init(model: FederationViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        NavigationSplitView {
            sidebar
                .navigationTitle("ios.federation.title")
                .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        } detail: {
            detail
        }
        .task { await model.load() }
    }

    private var sidebar: some View {
        SharingStateView(
            phase: model.phase,
            empty: .init(
                title: "ios.federation.empty.title",
                message: "ios.federation.empty.description",
                symbol: "square.stack.3d.up"
            ),
            retry: { Task { await model.reload() } },
            content: {
            List(model.albums, selection: $model.selection) { album in
                AggregatedAlbumRow(
                    album: album,
                    renderedCount: model.renderedAssetCount(in: album),
                    isDegraded: model.isDegraded(album),
                    hasUnreachableOwner: model.hasUnreachableOwner(album)
                )
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let album = model.selectedAlbum {
            FederationAlbumDetail(album: album, model: model)
        } else {
            ContentUnavailableView(
                "ios.federation.detail.none.title",
                systemImage: "square.stack.3d.up",
                description: Text("ios.federation.detail.none.description")
            )
        }
    }
}

// MARK: - AggregatedAlbumRow

/// One aggregated album in the list.
///
/// The count shown is what the local index still renders — **including** the
/// entries from origins that cannot be reached. A count that shrank while a
/// server was down would say "your photos were deleted", which is precisely the
/// wrong sentence.
struct AggregatedAlbumRow: View {
    let album: AggregatedAlbum
    let renderedCount: Int
    let isDegraded: Bool
    let hasUnreachableOwner: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            GroupNameText(name: album.groupName.value)
                .font(.headline)
            LabeledContent {
                Text(renderedCount, format: .number)
            } label: {
                Text("ios.federation.album.rendered_count")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            if hasUnreachableOwner {
                StatusBadge(
                    title: "ios.federation.album.unreachable_owner",
                    symbol: "externaldrive.badge.xmark",
                    tint: .orange
                )
            } else if isDegraded {
                StatusBadge(title: "ios.federation.album.partial", symbol: "exclamationmark.icloud", tint: .orange)
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - GroupNameText

/// An aggregated album's name, which converges by LWW across every
/// participant's assertion — and is therefore genuinely absent until somebody
/// has written one. A never-written register is not an empty string.
struct GroupNameText: View {
    let name: String?

    var body: some View {
        if let name, !name.isEmpty {
            Text(verbatim: name)
        } else {
            Text("ios.federation.album.untitled")
        }
    }
}

// MARK: - Previews

#Preview("Federation — degraded, light") {
    let environment = MockEnvironment(scenario: .degradedFederation)
    return FederationView(model: FederationViewModel(
        federation: environment.federation,
        moderation: environment.moderation,
        connectivity: SharingConnectivity(sync: environment.sync)
    ))
    .preferredColorScheme(.light)
}

#Preview("Federation — degraded, dark") {
    let environment = MockEnvironment(scenario: .degradedFederation)
    return FederationView(model: FederationViewModel(
        federation: environment.federation,
        moderation: environment.moderation,
        connectivity: SharingConnectivity(sync: environment.sync)
    ))
    .preferredColorScheme(.dark)
}

#Preview("Federation — healthy") {
    let environment = MockEnvironment(scenario: .healthy)
    return FederationView(model: FederationViewModel(
        federation: environment.federation,
        moderation: environment.moderation,
        connectivity: SharingConnectivity(sync: environment.sync)
    ))
}
