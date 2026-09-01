import CapsuleUI
import SwiftUI

// MARK: - Card

public extension View {
    /// The one card surface every identity screen groups related facts into.
    ///
    /// A plain filled shape rather than glass. Apple's guidance puts Liquid
    /// Glass on the control layer, and these cards sit *in* the content — a
    /// safety code, a word grid, a session row. Glass here would also sample the
    /// glass of the buttons beside it, which is the arrangement the HIG
    /// explicitly rules out.
    func authCard() -> some View {
        frame(maxWidth: .infinity, alignment: .leading)
            .padding(CapsuleTheme.Spacing.large)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large))
    }

    /// The same surface at row weight, for a list of devices or shares inside a
    /// card that is already a card.
    func authInnerCard() -> some View {
        frame(maxWidth: .infinity, alignment: .leading)
            .padding(CapsuleTheme.Spacing.medium)
            .background(.quinary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card))
    }
}

// MARK: - AuthLabeledValue

/// A translated label above a value that is **data, not copy**.
///
/// The split is the rule the whole module follows: the label comes from the
/// catalog, the value never does. Interpolating a value into a key would
/// produce a key no catalog contains, so values arrive here — or through
/// ``AuthCodeValue`` when a human has to compare them character by character.
struct AuthLabeledValue: View {
    let labelKey: LocalizedStringKey
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(labelKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: value)
                .font(.callout)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - AuthLabeledDate

/// A translated label above an instant, formatted by the system.
///
/// The date is never rendered into a catalog key: `Text(_:style:)` formats it in
/// the reader's locale and calendar, which a hand-built string could not do for
/// the fourteen locales this app ships.
struct AuthLabeledDate: View {
    let labelKey: LocalizedStringKey
    let date: Date

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(labelKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(date, style: .date)
                .font(.callout)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - AuthCodeValue

/// A code a human is expected to *compare*, rendered the one way that makes a
/// transposed character visible.
///
/// Monospaced, selectable, and never truncated. A proportional font makes
/// `0`/`O` and `1`/`l` indistinguishable in exactly the place that cannot afford
/// it, and a truncated fingerprint is a fingerprint that compares equal to
/// something it is not.
struct AuthCodeValue: View {
    let labelKey: LocalizedStringKey
    let code: String
    var font: Font = .body.monospaced()

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(labelKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: code)
                .font(font)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityLabel(labelKey)
                .accessibilityValue(Text(verbatim: code))
        }
    }
}

// MARK: - AuthSectionHeader

/// A step's heading inside a multi-step screen.
///
/// Numbered by the caller rather than by position in a `ForEach`, because a step
/// that becomes unreachable must keep its number — a restore whose dry run is
/// refused still has a step 3 the user is being told they cannot take.
struct AuthSectionHeader: View {
    let titleKey: LocalizedStringKey
    let descriptionKey: LocalizedStringKey
    var symbolName: String?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Label {
                Text(titleKey).font(.title3.weight(.semibold))
            } icon: {
                if let symbolName {
                    Image(systemName: symbolName).font(.title3)
                }
            }
            Text(descriptionKey)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}
