import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - FederationAlbumDetail

/// One aggregated album: its constituents, their origins, and the peers behind
/// them (*Federation — Federated Shared Albums*).
struct FederationAlbumDetail: View {
    let album: AggregatedAlbum
    let model: FederationViewModel

    var body: some View {
        Form {
            summarySection
            constituentSection
            peerSection
            scopeSection
        }
        .formStyle(.grouped)
        .frame(maxWidth: 720)
        .frame(maxWidth: .infinity)
        .navigationTitle("app.federation.detail.title")
    }

    // MARK: Sections

    /// The headline reassurance: how much this album still shows, and — when
    /// something is down — that the shortfall is reach, not loss.
    private var summarySection: some View {
        Section {
            GroupNameText(name: album.groupName.value)
                .font(.headline)
            LabeledContent {
                Text(model.renderedAssetCount(in: album), format: .number)
            } label: {
                Text("app.federation.album.rendered_count")
            }
            if model.hasUnreachableOwner(album) {
                Label("app.federation.state.unreachable_owner", systemImage: "externaldrive.badge.xmark")
                    .foregroundStyle(.orange)
                    .accessibilityElement(children: .combine)
            }
        } footer: {
            Text(model.isDegraded(album)
                ? "app.federation.summary.partial_footer"
                : "app.federation.summary.complete_footer")
        }
    }

    private var constituentSection: some View {
        Section {
            ForEach(album.constituents) { ConstituentRow(constituent: $0) }
        } header: {
            Text("app.federation.section.constituents")
        } footer: {
            Text("app.federation.section.constituents_footer")
        }
    }

    @ViewBuilder
    private var peerSection: some View {
        let relevant = model.peers.filter { peer in
            album.constituents.contains { $0.peerID == peer.id }
        }
        if !relevant.isEmpty {
            Section("app.federation.section.peers") {
                ForEach(relevant) { peer in
                    FederationPeerRow(
                        peer: peer,
                        availability: model.availability(of: peer),
                        compartment: model.compartments[peer.id]
                    )
                }
            }
        }
    }

    /// The two things a user will otherwise assume wrongly: that leaving
    /// removes their photographs from other people's copies, and that there is
    /// some way to remove somebody else's.
    private var scopeSection: some View {
        Section("app.federation.section.how") {
            ScopeNote(message: "app.federation.note.no_kick")
            ScopeNote(message: "app.federation.note.never_removed")
            ScopeNote(message: "app.federation.note.auto_recovery")
        }
    }
}

// MARK: - ConstituentRow

/// One contributor's container album inside the aggregate.
///
/// The origin is rendered verbatim and monospaced immediately above its state,
/// so the pairing reads as "photos from *this server* are currently
/// unavailable" — the exact sentence the design specifies, without embedding a
/// hostname inside a translated string.
struct ConstituentRow: View {
    let constituent: AggregatedConstituent

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(verbatim: constituent.homeServer)
                .font(.subheadline.monospaced())
            statusLabel
            LabeledContent {
                Text(constituent.assetCount, format: .number)
            } label: {
                Text("app.federation.constituent.count")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var statusLabel: some View {
        switch constituent.availability {
        case .available:
            StatusBadge(title: "app.federation.origin.available", symbol: "checkmark.icloud", tint: .green)
        case .temporarilyUnreachable:
            StatusBadge(
                title: "app.federation.origin.unavailable",
                symbol: "exclamationmark.icloud",
                tint: .orange
            )
        case .ownedByUnreachableServer:
            StatusBadge(
                title: "app.federation.origin.unreachable_owner",
                symbol: "externaldrive.badge.xmark",
                tint: .orange
            )
        case .blocked:
            StatusBadge(title: "app.federation.origin.blocked", symbol: "hand.raised", tint: .red)
        }
    }
}

// MARK: - FederationPeerRow

/// One peer server, with its containment budgets.
///
/// The budgets are shown because they are the honest answer to "why is this
/// slow": transfer and storage are bounded separately and per peer, so a busy
/// peer is throttled rather than allowed to starve the others.
struct FederationPeerRow: View {
    let peer: Peer
    let availability: PeerAvailability
    let compartment: PeerCompartment?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(verbatim: peer.origin)
                .font(.subheadline.monospaced())
            availabilityBadge
            if let lastPull = peer.lastSuccessfulPullAt {
                LabeledContent {
                    Text(lastPull.date, format: .relative(presentation: .named))
                } label: {
                    Text("app.federation.peer.last_pull")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            if let compartment {
                CompartmentSummary(compartment: compartment)
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var availabilityBadge: some View {
        switch availability {
        case .reachable:
            StatusBadge(title: "app.federation.peer.reachable", symbol: "checkmark.icloud", tint: .green)
        case .transientOutage:
            StatusBadge(title: "app.federation.peer.transient", symbol: "exclamationmark.icloud", tint: .orange)
        case .backingOff:
            StatusBadge(title: "app.federation.peer.backing_off", symbol: "clock.arrow.circlepath", tint: .orange)
        case .unreachableServer:
            StatusBadge(title: "app.federation.peer.unreachable", symbol: "externaldrive.badge.xmark", tint: .orange)
        case .blocked:
            StatusBadge(title: "app.federation.peer.blocked", symbol: "hand.raised", tint: .red)
        case .unknown:
            StatusBadge(title: "app.federation.peer.unknown", symbol: "questionmark.circle", tint: .secondary)
        }
    }
}

// MARK: - CompartmentSummary

/// One peer's cache budget, charged to the **receiving** user's quota.
struct CompartmentSummary: View {
    let compartment: PeerCompartment

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            LabeledContent {
                Text(Int64(compartment.cachedBytes), format: .byteCount(style: .file))
            } label: {
                Text("app.federation.compartment.cached")
            }
            if compartment.isCacheBudgetExhausted {
                StatusBadge(
                    title: "app.federation.compartment.exhausted",
                    symbol: "externaldrive.badge.exclamationmark",
                    tint: .orange
                )
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}
