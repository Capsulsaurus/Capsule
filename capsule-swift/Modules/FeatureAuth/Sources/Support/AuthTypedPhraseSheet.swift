import CapsuleUI
import SwiftUI

// MARK: - AuthTypedPhraseSheet

/// The typed-phrase confirmation for a ceremony a button press cannot honestly
/// authorise.
///
/// *Backup & Recovery* requires exactly this for committing a restore: a
/// `restore` runs in dry-run mode unless the user passes `--commit`, "or its UI
/// equivalent: a confirm-with-typed-phrase dialog after the dry-run report is
/// shown". A whole library is reconciled against escrowed state, and a mis-tap
/// is not a recoverable input.
///
/// It is the same shape as the settings module's sheet and deliberately **not**
/// the same code. `FeatureAuth` does not depend on `FeatureSettings`, and the
/// two differ in the one place that matters: the settings gate resolves its
/// phrase *from the catalog*, whereas the phrase here is a fixed token the
/// caller owns and shows with `Text(verbatim:)`. Translating it would make the
/// required keystrokes depend on the app's language, which is precisely the
/// variability a confirmation gate must not have. Sharing one type would have
/// meant a parameter that switches between those two contracts — one more thing
/// to get wrong on the screen that can least afford it.
///
/// The comparison itself lives in the caller's view model, so the sheet cannot
/// disagree with the model about whether the gate has passed.
struct AuthTypedPhraseSheet: View {
    let titleKey: LocalizedStringKey
    let messageKey: LocalizedStringKey
    let fieldLabelKey: LocalizedStringKey
    let confirmKey: LocalizedStringKey
    /// The phrase to copy. Shown, not translated.
    let requiredPhrase: String
    @Binding var typedPhrase: String
    /// Whether the caller's model considers the gate passed.
    let isSatisfied: Bool
    let confirm: @MainActor () async -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: titleKey,
                    subtitleKey: messageKey,
                    symbolName: "exclamationmark.shield.fill"
                )
                phraseCard
                field
                actions
            }
        }
        .authSheetFrame()
    }

    private var phraseCard: some View {
        AuthCodeValue(
            labelKey: "app.auth.confirm.phrase.required",
            code: requiredPhrase,
            font: .title3.monospaced()
        )
        .authCard()
    }

    private var field: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(fieldLabelKey)
                .font(.headline)
            TextField(fieldLabelKey, text: $typedPhrase)
                .textFieldStyle(.roundedBorder)
                .font(.body.monospaced())
                .autocorrectionDisabled()
                .accessibilityLabel(fieldLabelKey)
            if !typedPhrase.isEmpty, !isSatisfied {
                StatusChip(
                    titleKey: "app.auth.confirm.phrase.mismatch",
                    symbolName: "exclamationmark.triangle.fill",
                    tint: .orange
                )
            }
            Text("app.auth.confirm.phrase.footer")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var actions: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Button(confirmKey, role: .destructive) {
                Task {
                    await confirm()
                    dismiss()
                }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(!isSatisfied)
            .accessibilityLabel(confirmKey)

            Button("app.common.cancel", role: .cancel) { dismiss() }
                .buttonStyle(.borderless)
                .accessibilityLabel("app.common.cancel")
        }
    }
}
