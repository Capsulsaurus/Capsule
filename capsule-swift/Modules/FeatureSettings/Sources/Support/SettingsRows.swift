import SwiftUI

// MARK: - SettingsTone

/// How urgent a status row is.
///
/// A tone always resolves to **both** a tint and a symbol, and is only ever
/// rendered next to its own text. That pairing is the rule the accessibility
/// audit enforces: colour may reinforce a status but must never be the only
/// thing carrying it, because a red dot and a green dot are the same dot to a
/// large minority of users and to every VoiceOver listener.
public enum SettingsTone: Sendable, Equatable, CaseIterable {
    case neutral
    case positive
    case caution
    case critical

    /// The tint. Reinforcement only — never the sole signal.
    public var tint: Color {
        switch self {
        case .neutral: .secondary
        case .positive: .green
        case .caution: .orange
        case .critical: .red
        }
    }

    /// The symbol that carries the same meaning as ``tint`` for anyone who
    /// cannot use the tint.
    public var symbol: String {
        switch self {
        case .neutral: "circle"
        case .positive: "checkmark.circle.fill"
        case .caution: "exclamationmark.triangle.fill"
        case .critical: "xmark.octagon.fill"
        }
    }
}

// MARK: - SettingsValueRow

/// A label from the catalog paired with a value that is data, not copy.
///
/// The split matters: the label is translated, the value never is. A row that
/// interpolated the value into the key would produce a key no catalog contains,
/// which is why every value on every screen in this module arrives through
/// here or through ``SettingsStatusRow``.
public struct SettingsValueRow: View {
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
                .multilineTextAlignment(.trailing)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - SettingsStatusRow

/// A label paired with a toned status word.
///
/// The status is text first: the symbol and the tint are added to it, never
/// substituted for it, so the row survives greyscale, Reduce Transparency, and
/// VoiceOver unchanged.
public struct SettingsStatusRow: View {
    private let labelKey: String
    private let statusKey: String
    private let tone: SettingsTone

    public init(labelKey: String, statusKey: String, tone: SettingsTone) {
        self.labelKey = labelKey
        self.statusKey = statusKey
        self.tone = tone
    }

    public var body: some View {
        LabeledContent(LocalizedStringKey(labelKey)) {
            Label(LocalizedStringKey(statusKey), systemImage: tone.symbol)
                .labelStyle(.titleAndIcon)
                .foregroundStyle(tone.tint)
                .imageScale(.small)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - SettingsNoteRow

/// A paragraph of explanatory copy inside a form section.
///
/// Used where a footer will not do — an explanation that belongs to one row
/// rather than to the section. Secondary and small-caption sized, but it scales
/// with Dynamic Type like any other text and is never truncated.
public struct SettingsNoteRow: View {
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
