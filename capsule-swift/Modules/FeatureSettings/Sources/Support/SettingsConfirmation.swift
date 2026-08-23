import Foundation
import Observation
import SwiftUI

// MARK: - SettingsPhrase

/// Catalog lookup for the one case where a translated string must be
/// *compared* rather than displayed: the typed-phrase gate.
///
/// A user confirming a restore in French types the French word, so the gate has
/// to know what the catalog resolved to at runtime. Everywhere else in this
/// module a key goes straight into a `Text` and is never materialised.
public enum SettingsPhrase {
    public static func text(forKey key: String) -> String {
        String(localized: String.LocalizationValue(key))
    }
}

// MARK: - TypedPhraseGate

/// The typed-phrase confirmation for a ceremony that a button press cannot
/// honestly authorise.
///
/// *Backup & Recovery* requires this specifically for committing a restore:
/// "a `restore` invocation runs in dry-run mode unless the user passes an
/// explicit `--commit` flag (or its UI equivalent: a confirm-with-typed-phrase
/// dialog after the dry-run report is shown)". A restore reconciles a whole
/// library against escrowed state, and a mis-tap is not a recoverable input.
///
/// The comparison trims surrounding whitespace — a keyboard's trailing space or
/// a paste with a newline is not a different intent — and is otherwise
/// **exact, case included**. Loosening it would make the gate a formality, and
/// the gate exists precisely because a formality is what a button already was.
@MainActor
@Observable
public final class TypedPhraseGate {
    /// The phrase, already resolved from the catalog by the caller.
    public let requiredPhrase: String
    /// What the user has typed so far.
    public var typedPhrase = ""

    public init(requiredPhrase: String) {
        self.requiredPhrase = requiredPhrase
    }

    /// Build the gate from a catalog key.
    public convenience init(phraseKey: String) {
        self.init(requiredPhrase: SettingsPhrase.text(forKey: phraseKey))
    }

    /// Whether the ceremony may proceed.
    public var isSatisfied: Bool {
        !requiredPhrase.isEmpty && normalised(typedPhrase) == normalised(requiredPhrase)
    }

    /// Whether the user has started typing but has not matched yet — the state
    /// an inline hint should appear in, rather than shouting at an empty field.
    public var isPartiallyTyped: Bool {
        !typedPhrase.isEmpty && !isSatisfied
    }

    /// Clear the field, so re-opening the dialog never starts pre-satisfied.
    public func reset() {
        typedPhrase = ""
    }

    private func normalised(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

// MARK: - TypedPhraseConfirmationSheet

/// The sheet that hosts a ``TypedPhraseGate``.
///
/// The confirm button stays disabled until the phrase matches, and the required
/// phrase is shown so the user is copying rather than guessing — the goal is
/// deliberateness, not recall.
public struct TypedPhraseConfirmationSheet: View {
    private let titleKey: String
    private let messageKey: String
    private let fieldLabelKey: String
    private let confirmKey: String
    private let gate: TypedPhraseGate
    private let confirm: @MainActor () async -> Void

    @Environment(\.dismiss) private var dismiss

    public init(
        titleKey: String,
        messageKey: String,
        fieldLabelKey: String,
        confirmKey: String,
        gate: TypedPhraseGate,
        confirm: @escaping @MainActor () async -> Void
    ) {
        self.titleKey = titleKey
        self.messageKey = messageKey
        self.fieldLabelKey = fieldLabelKey
        self.confirmKey = confirmKey
        self.gate = gate
        self.confirm = confirm
    }

    public var body: some View {
        @Bindable var boundGate = gate
        return Form {
            Section {
                Text(LocalizedStringKey(messageKey))
                    .fixedSize(horizontal: false, vertical: true)
                SettingsValueRow(
                    labelKey: "app.settings.confirm.phrase.required",
                    value: gate.requiredPhrase
                )
                TextField(
                    LocalizedStringKey(fieldLabelKey),
                    text: $boundGate.typedPhrase
                )
                .textContentType(.none)
                .accessibilityLabel(Text(LocalizedStringKey(fieldLabelKey)))
                if gate.isPartiallyTyped {
                    Label(
                        "app.settings.confirm.phrase.mismatch",
                        systemImage: SettingsTone.caution.symbol
                    )
                    .foregroundStyle(SettingsTone.caution.tint)
                    .font(.footnote)
                }
            } footer: {
                Text("app.settings.confirm.phrase.footer")
            }

            Section {
                Button(LocalizedStringKey(confirmKey), role: .destructive) {
                    Task {
                        await confirm()
                        gate.reset()
                        dismiss()
                    }
                }
                .disabled(!gate.isSatisfied)
                Button("app.settings.confirm.cancel", role: .cancel) {
                    gate.reset()
                    dismiss()
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle(LocalizedStringKey(titleKey))
    }
}

// MARK: - Destructive confirmation

public extension View {
    /// The ordinary confirmation every destructive settings action carries.
    ///
    /// A dialog rather than an alert because the action is the subject: the
    /// button says what will happen, so a user who reads only the button is
    /// still told. Ceremonies whose cost is a whole library — a restore commit —
    /// use ``TypedPhraseConfirmationSheet`` instead.
    func settingsDestructiveConfirmation(
        titleKey: String,
        messageKey: String,
        confirmKey: String,
        isPresented: Binding<Bool>,
        action: @escaping @MainActor () async -> Void
    ) -> some View {
        confirmationDialog(
            LocalizedStringKey(titleKey),
            isPresented: isPresented,
            titleVisibility: .visible
        ) {
            Button(LocalizedStringKey(confirmKey), role: .destructive) {
                Task { await action() }
            }
            Button("app.settings.confirm.cancel", role: .cancel) {}
        } message: {
            Text(LocalizedStringKey(messageKey))
        }
    }
}
