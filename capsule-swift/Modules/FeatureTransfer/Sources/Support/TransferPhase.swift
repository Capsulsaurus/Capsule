import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - ScreenPhase

/// Where a transfer screen stands: the four states every screen in this module
/// must be able to render.
///
/// Offline is a *first-class phase*, not a flavour of failure. The offline-first
/// contract (*Download and Synchronization — Auto Syncing*) is that local reads
/// keep answering with no network, so a screen that is offline has usually
/// loaded perfectly well and simply cannot act. Collapsing that into
/// ``failed(_:)`` would train users to read a working library as broken.
public enum ScreenPhase: Sendable, Equatable {
    /// The first read has not returned yet.
    case loading
    /// Data is present and the screen is interactive.
    case ready
    /// The read succeeded and there is genuinely nothing to show. For several
    /// screens here — quarantine above all — this is the *good* state.
    case empty
    /// No usable connection. Locally-derived content is still shown; anything
    /// that needs the server is disabled rather than hidden, so the user can
    /// see what would happen once they are back on a link.
    case offline
    /// A port refused. The stable ``ErrorCode`` is what the user-facing string
    /// is looked up by; the English `detail` never reaches the screen.
    case failed(CapsuleError)

    /// Whether the screen has data worth drawing behind any banner.
    public var hasContent: Bool {
        self == .ready || self == .offline
    }

    /// Whether an action that needs the server may be offered.
    public var permitsNetworkActions: Bool {
        self != .offline
    }
}

// MARK: - PhasePlaceholderView

/// The shared loading / empty / offline / failure surface.
///
/// One view rather than four per screen so the wording, the symbol pairing, and
/// the retry affordance cannot drift between screens — and so that "colour is
/// never the only signal" is satisfied once: every phase pairs a tint with a
/// symbol *and* text.
public struct PhasePlaceholderView: View {
    private let phase: ScreenPhase
    private let emptyTitle: LocalizedStringKey
    private let emptyDescription: LocalizedStringKey
    private let emptySymbol: String
    private let retry: () async -> Void

    public init(
        phase: ScreenPhase,
        emptyTitle: LocalizedStringKey,
        emptyDescription: LocalizedStringKey,
        emptySymbol: String,
        retry: @escaping () async -> Void
    ) {
        self.phase = phase
        self.emptyTitle = emptyTitle
        self.emptyDescription = emptyDescription
        self.emptySymbol = emptySymbol
        self.retry = retry
    }

    public var body: some View {
        switch phase {
        case .loading:
            ProgressView()
                .controlSize(.large)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel("app.transfer.state.loading")
        case .empty:
            ContentUnavailableView(
                emptyTitle,
                systemImage: emptySymbol,
                description: Text(emptyDescription)
            )
        case .offline:
            ContentUnavailableView {
                Label("app.transfer.state.offline.title", systemImage: "wifi.slash")
            } description: {
                Text("app.transfer.state.offline.description")
            } actions: {
                retryButton
            }
        case let .failed(error):
            ContentUnavailableView {
                Label("app.transfer.state.error.title", systemImage: "exclamationmark.triangle")
            } description: {
                Text(LocalizedStringKey(error.localizationKey))
            } actions: {
                retryButton
            }
        case .ready:
            EmptyView()
        }
    }

    private var retryButton: some View {
        Button("app.transfer.action.retry") {
            Task { await retry() }
        }
        .capsuleGlassButtonStyle(prominent: true)
    }
}

// MARK: - Phase resolution

public extension ScreenPhase {
    /// Classify a thrown port error into a phase.
    ///
    /// A connection class the caller already knows is offline wins over the
    /// code, because a refusal *caused by* being offline is not a server
    /// rejection the user can act on.
    static func resolve(_ error: any Error, connection: ConnectionClass) -> ScreenPhase {
        guard connection.isUsable else { return .offline }
        guard let capsuleError = error as? CapsuleError else {
            return .failed(CapsuleError(code: .unknown("error.unexpected"), detail: "\(error)"))
        }
        return .failed(capsuleError)
    }
}
