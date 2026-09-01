import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - SharingStateView

/// Renders a ``SharingPhase`` and defers to `content` once there is something
/// to show.
///
/// One component rather than nine hand-rolled `if` ladders so that the four
/// non-content states cannot drift apart between screens — in particular so
/// that "empty" never accidentally renders as an error, which for a drop inbox
/// or a peer list is the difference between "nothing has arrived" and
/// "something is broken".
struct SharingStateView<Content: View>: View {
    let phase: SharingPhase
    let empty: SharingEmptyState
    let retry: (() -> Void)?
    @ViewBuilder let content: () -> Content

    var body: some View {
        switch phase {
        case .loading:
            ProgressView()
                .controlSize(.large)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel("app.share.state.loading")
        case .ready:
            content()
        case .empty:
            ContentUnavailableView(empty.title, systemImage: empty.symbol, description: Text(empty.message))
        case .offline:
            ContentUnavailableView(
                "app.share.state.offline.title",
                systemImage: "wifi.slash",
                description: Text("app.share.state.offline.description")
            )
        case let .failed(code):
            failure(code)
        }
    }

    /// The failure state renders the **catalog message for the code**, never
    /// ``CapsuleError/detail`` — that field is an English diagnostic for logs
    /// and support reports, and showing it to a user is a localisation bug.
    private func failure(_ code: ErrorCode) -> some View {
        ContentUnavailableView {
            Label("app.share.state.error.title", systemImage: "exclamationmark.triangle")
        } description: {
            Text(LocalizedStringKey(code.rawValue))
        } actions: {
            if let retry {
                Button("app.share.action.retry", action: retry)
                    .capsuleGlassButtonStyle(prominent: true)
            }
        }
    }
}

// MARK: - SharingEmptyState

/// The copy for one screen's empty state.
///
/// Per-screen rather than shared: the reason a drop inbox is empty ("nobody has
/// sent you anything") has nothing in common with the reason a peer list is
/// ("no other device of yours is on this network right now"), and a generic
/// "No items" would make the second one read as a fault.
struct SharingEmptyState {
    let title: LocalizedStringKey
    let message: LocalizedStringKey
    let symbol: String
}

// MARK: - StatusBadge

/// A small state marker: symbol, text, and a tint — in that order of
/// importance.
///
/// The symbol and the text carry the meaning; the tint only reinforces it. The
/// accessibility audit rejects colour as the sole signal, and so does anyone
/// reading the screen in bright sun.
struct StatusBadge: View {
    let title: LocalizedStringKey
    let symbol: String
    let tint: Color

    var body: some View {
        Label(title, systemImage: symbol)
            .font(.caption)
            .foregroundStyle(tint)
            .labelStyle(.titleAndIcon)
            .accessibilityElement(children: .combine)
    }
}

// MARK: - OfflineNotice

/// The banner shown above content that loaded from the local index while the
/// device has no usable path.
///
/// Content-first: the rows stay, the notice explains why nothing new is
/// arriving. Capsule is offline-first, so a local read succeeding with no
/// network is the normal case, not a failure worth hiding the screen for.
struct OfflineNotice: View {
    let connection: ConnectionClass?

    var body: some View {
        if let connection, !connection.isUsable {
            Label("app.share.offline_notice", systemImage: "wifi.slash")
                .font(.footnote)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(CapsuleTheme.Spacing.small)
                .capsuleGlass(in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.small))
                .accessibilityElement(children: .combine)
        }
    }
}

// MARK: - ScopeNote

/// A non-interactive explanatory row.
///
/// Used wherever the honest answer is "the product does not do this", which in
/// this group of screens is often: writable share links and per-recipient
/// analytics are out of v1 scope (*Share Links*), and there is no group-level
/// kick in a federated album (*Federation*). Saying so costs one row and stops
/// a user hunting for a control that was never built.
struct ScopeNote: View {
    let message: LocalizedStringKey

    var body: some View {
        Label(message, systemImage: "info.circle")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
    }
}
