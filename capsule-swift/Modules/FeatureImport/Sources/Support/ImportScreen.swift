import CapsuleDomain
import SwiftUI

// MARK: - ImportScreen

/// The scaffold every import screen is drawn inside.
///
/// It owns the four non-content states — loading, empty, offline, failed — so
/// that no screen can ship without them and all five agree on what "offline"
/// looks like. A screen supplies only its content and the key for its own empty
/// message.
///
/// Each state's body is a separate small view rather than one branching
/// expression: Swift 6.3's IRGen has been observed to fail on very large single
/// expressions, and a scaffold that every screen in the module funnels through
/// is the last place that should be discovered.
public struct ImportScreen<Content: View>: View {
    private let titleKey: String
    private let phase: ImportPhase
    private let emptyTitleKey: String
    private let emptyDescriptionKey: String
    private let emptySymbol: String
    private let retry: @MainActor () async -> Void
    private let content: () -> Content

    public init(
        titleKey: String,
        phase: ImportPhase,
        emptyTitleKey: String,
        emptyDescriptionKey: String,
        emptySymbol: String = "tray",
        retry: @escaping @MainActor () async -> Void,
        @ViewBuilder content: @escaping () -> Content
    ) {
        self.titleKey = titleKey
        self.phase = phase
        self.emptyTitleKey = emptyTitleKey
        self.emptyDescriptionKey = emptyDescriptionKey
        self.emptySymbol = emptySymbol
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
        case .loading: loadingBody
        case .empty: emptyBody
        case .offline: offlineBody
        case let .failed(code): failureBody(code)
        case .ready: content()
        }
    }

    private var loadingBody: some View {
        ProgressView(LocalizedStringKey("ios.import.state.loading"))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityLabel(Text("ios.import.state.loading"))
    }

    private var emptyBody: some View {
        ContentUnavailableView {
            Label(LocalizedStringKey(emptyTitleKey), systemImage: emptySymbol)
        } description: {
            Text(LocalizedStringKey(emptyDescriptionKey))
        } actions: {
            retryButton
        }
    }

    private var offlineBody: some View {
        ContentUnavailableView {
            Label("ios.import.state.offline.title", systemImage: "wifi.slash")
        } description: {
            Text("ios.import.state.offline.description")
        } actions: {
            retryButton
        }
    }

    /// Renders the code's own catalog message, never the English `detail` string
    /// on ``CapsuleError`` — that field is a diagnostic for logs and support
    /// bundles, and putting it on screen is a localisation bug rather than a
    /// helpful extra.
    private func failureBody(_ code: ErrorCode) -> some View {
        ContentUnavailableView {
            Label("ios.import.state.error.title", systemImage: "exclamationmark.triangle")
        } description: {
            Text(LocalizedStringKey(code.rawValue))
        } actions: {
            retryButton
        }
    }

    private var retryButton: some View {
        Button("ios.import.state.retry") {
            Task { await retry() }
        }
        .buttonStyle(.borderedProminent)
    }
}

// MARK: - ImportStatusLabel

/// A toned status word.
///
/// The status is text first: the symbol and the tint are added to it, never
/// substituted for it, so the label survives greyscale, Reduce Transparency, and
/// VoiceOver unchanged.
public struct ImportStatusLabel: View {
    private let titleKey: String
    private let tone: ImportTone
    private let symbol: String?

    public init(titleKey: String, tone: ImportTone, symbol: String? = nil) {
        self.titleKey = titleKey
        self.tone = tone
        self.symbol = symbol
    }

    public var body: some View {
        Label(LocalizedStringKey(titleKey), systemImage: symbol ?? tone.symbol)
            .labelStyle(.titleAndIcon)
            .foregroundStyle(tone.tint)
            .imageScale(.small)
    }
}

// MARK: - ImportValueRow

/// A label from the catalog paired with a value that is data, not copy.
///
/// The split matters: the label is translated, the value never is. A row that
/// interpolated the value into the key would produce a key no catalog contains.
public struct ImportValueRow: View {
    private let labelKey: String
    private let value: String

    public init(labelKey: String, value: String) {
        self.labelKey = labelKey
        self.value = value
    }

    public var body: some View {
        LabeledContent(LocalizedStringKey(labelKey)) {
            Text(verbatim: value)
                .foregroundStyle(.secondary)
                .monospacedDigit()
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - ImportNote

/// A paragraph of explanatory copy inside a section.
///
/// Secondary and caption-sized, but it scales with Dynamic Type like any other
/// text and is never truncated.
public struct ImportNote: View {
    private let textKey: String

    public init(textKey: String) {
        self.textKey = textKey
    }

    public var body: some View {
        Text(LocalizedStringKey(textKey))
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}
