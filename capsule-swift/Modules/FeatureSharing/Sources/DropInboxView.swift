import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - DropInboxView

/// Guest uploads awaiting review (*Web Upload*).
///
/// A `NavigationSplitView` so the same code is a push-stack on iPhone and a
/// list-plus-detail on iPad and Mac, without a size-class branch: the review
/// decision needs the card and the destination picker visible together wherever
/// there is room for both.
public struct DropInboxView: View {
    @State private var model: DropInboxViewModel
    private let drops: any DropPort
    private let albums: any AlbumPort
    private let connectivity: SharingConnectivity

    public init(
        drops: any DropPort,
        albums: any AlbumPort,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        _model = State(wrappedValue: DropInboxViewModel(drops: drops, connectivity: connectivity))
        self.drops = drops
        self.albums = albums
        self.connectivity = connectivity
    }

    public var body: some View {
        NavigationSplitView {
            sidebar
                .navigationTitle("app.drops.inbox.title")
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
                title: "app.drops.inbox.empty.title",
                message: "app.drops.inbox.empty.description",
                symbol: "tray"
            ),
            retry: { Task { await model.reload() } },
            content: {
                List(model.drops, selection: $model.selection) { drop in
                    DropCardView(drop: drop).tag(drop.id)
                }
            }
        )
    }

    @ViewBuilder
    private var detail: some View {
        if let drop = model.selected {
            DropDetailView(
                model: DropDetailViewModel(
                    drop: drop,
                    drops: drops,
                    albums: albums,
                    connectivity: connectivity
                ),
                onFinish: { Task { await model.reload() } }
            )
            .id(drop.id)
        } else {
            ContentUnavailableView(
                "app.drops.detail.none.title",
                systemImage: "hand.tap",
                description: Text("app.drops.detail.none.description")
            )
        }
    }
}

// MARK: - DropCardView

/// One drop, as a claim.
///
/// The server-attested arrival time is the only trustworthy field on the card;
/// everything the guest wrote is behind an "unverified" marker. The filename in
/// particular is quoted so it can only read as *something a stranger typed*,
/// never as a title this app chose — the mock inbox deliberately contains
/// `"Settings — Capsule.png"` to keep that honest.
struct DropCardView: View {
    let drop: PendingDrop

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            HStack(spacing: CapsuleTheme.Spacing.small) {
                Image(systemName: symbol)
                    .foregroundStyle(.secondary)
                Text("app.drops.card.awaiting_review")
                    .font(.headline)
                Spacer(minLength: CapsuleTheme.Spacing.small)
            }
            LabeledContent {
                Text(drop.receivedAt.date, format: .dateTime.year().month().day().hour().minute())
            } label: {
                Text("app.drops.card.received")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            LabeledContent {
                Text(verbatim: shortLinkID)
                    .monospaced()
            } label: {
                Text("app.drops.card.via_link")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            UnverifiedClaimView(claim: GuestClaim.quoted(drop.suggestedFilename))
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }

    /// The tail of the owner-held revocation handle. Enough to tell two links
    /// apart in a list; not the URL's opaque id, which is a secret.
    private var shortLinkID: String {
        String(drop.viaLink.rawValue.suffix(8))
    }

    private var symbol: String {
        drop.descriptor.contentType.mediaKind == .video ? "film" : "photo"
    }
}

// MARK: - UnverifiedClaimView

/// A guest-asserted string, marked as one.
///
/// The marker comes **first** so it is read before the claim, in visual order
/// and in VoiceOver order. A badge trailing the text would be seen after the
/// reader has already believed it.
struct UnverifiedClaimView: View {
    let claim: String?

    var body: some View {
        if let claim {
            HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.xSmall) {
                Label("app.drops.claim.unverified", systemImage: "questionmark.circle")
                    .font(.caption2)
                    .foregroundStyle(.orange)
                Text(verbatim: claim)
                    .font(.caption)
                    .italic()
                    .lineLimit(2)
            }
            .accessibilityElement(children: .combine)
        } else {
            Label("app.drops.claim.none", systemImage: "questionmark.circle")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Previews

#Preview("Drop inbox — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return DropInboxView(
        drops: environment.drops,
        albums: environment.albums,
        connectivity: SharingConnectivity(sync: environment.sync)
    )
    .preferredColorScheme(.light)
}

#Preview("Drop inbox — dark, offline") {
    let environment = MockEnvironment(scenario: .offline)
    return DropInboxView(
        drops: environment.drops,
        albums: environment.albums,
        connectivity: SharingConnectivity(sync: environment.sync)
    )
    .preferredColorScheme(.dark)
}
