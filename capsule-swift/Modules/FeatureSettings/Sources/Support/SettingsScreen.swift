import CapsuleDomain
import SwiftUI

// MARK: - SettingsScreen

/// The scaffold every settings screen is drawn inside.
///
/// It owns the four non-content states — loading, empty, offline, failed — so
/// that no screen can accidentally ship without them, and so all eighteen agree
/// on what "offline" looks like. A screen supplies only its content and the key
/// for its own empty message; the rest is not a per-screen decision.
///
/// `Form` with an explicit `.grouped` style rather than the platform default:
/// on iOS the default is already grouped, on macOS it is not, and the Mac
/// preference panes in this module are sized to their content, which grouped
/// rows do and columns do not.
public struct SettingsScreen<Content: View>: View {
    private let titleKey: String
    private let phase: SettingsPhase
    private let emptyTitleKey: String
    private let emptyDescriptionKey: String
    private let retry: @MainActor () async -> Void
    private let content: () -> Content

    public init(
        titleKey: String,
        phase: SettingsPhase,
        emptyTitleKey: String = "app.settings.state.empty.title",
        emptyDescriptionKey: String = "app.settings.state.empty.description",
        retry: @escaping @MainActor () async -> Void,
        @ViewBuilder content: @escaping () -> Content
    ) {
        self.titleKey = titleKey
        self.phase = phase
        self.emptyTitleKey = emptyTitleKey
        self.emptyDescriptionKey = emptyDescriptionKey
        self.retry = retry
        self.content = content
    }

    public var body: some View {
        stateBody
            .navigationTitle(LocalizedStringKey(titleKey))
    }

    @ViewBuilder
    private var stateBody: some View {
        switch phase {
        case .loading:
            ProgressView(LocalizedStringKey("app.settings.state.loading"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel(Text("app.settings.state.loading"))
        case .empty:
            unavailable(
                titleKey: emptyTitleKey,
                symbol: "tray",
                descriptionKey: emptyDescriptionKey
            )
        case .offline:
            unavailable(
                titleKey: "app.settings.state.offline.title",
                symbol: "wifi.slash",
                descriptionKey: "app.settings.state.offline.description"
            )
        case let .failed(code):
            failure(code: code)
        case .ready:
            Form { content() }
                .formStyle(.grouped)
        }
    }

    private func unavailable(
        titleKey: String,
        symbol: String,
        descriptionKey: String
    ) -> some View {
        ContentUnavailableView {
            Label(LocalizedStringKey(titleKey), systemImage: symbol)
        } description: {
            Text(LocalizedStringKey(descriptionKey))
        } actions: {
            retryButton
        }
    }

    /// The failure state renders the code's own catalog message, never the
    /// English `detail` string on ``CapsuleError`` — that field is a diagnostic
    /// for logs and support bundles, and putting it on screen is a localisation
    /// bug rather than a helpful extra.
    private func failure(code: ErrorCode) -> some View {
        ContentUnavailableView {
            Label("app.settings.state.error.title", systemImage: "exclamationmark.triangle")
        } description: {
            Text(LocalizedStringKey(code.rawValue))
        } actions: {
            retryButton
        }
    }

    private var retryButton: some View {
        Button("app.settings.state.retry") {
            Task { await retry() }
        }
        .accessibilityLabel(Text("app.settings.state.retry"))
    }
}
