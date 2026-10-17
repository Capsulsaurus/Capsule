import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - RecoveryPassphraseView

/// The recovery phrase: shown once, then typed back.
///
/// **There is no skip.** No "remind me later", no "I'll do this in Settings", no
/// dismiss gesture past the gate. *Device Enrollment* step 6 gates setup on the
/// type-back precisely so the phrase is recorded rather than dismissed, and this
/// screen has no affordance that would undo that — the view model deliberately
/// offers no method to call if it did.
///
/// The secret lives in the view model for as long as this screen is on-screen
/// and is dropped the moment the gate passes. Nothing here writes it anywhere:
/// the words are read from ``RecoveryPassphraseViewModel/revealedWords`` at
/// render time, and the one plaintext egress is Copy, which the user asked for.
///
/// Entry point: ``init(recovery:source:onComplete:)``, needing ``RecoveryPort``.
public struct RecoveryPassphraseView: View {
    @State private var model: RecoveryPassphraseViewModel
    /// What the user has typed, per word position. Local to the screen and gone
    /// with it — the answers are checked through the secret, never stored beside
    /// it.
    @State private var answers: [Int: String] = [:]
    private let onComplete: () -> Void

    public init(
        recovery: any RecoveryPort,
        source: RecoverySecretSource = .setUp,
        onComplete: @escaping () -> Void = {}
    ) {
        _model = State(wrappedValue: RecoveryPassphraseViewModel(recovery: recovery, source: source))
        self.onComplete = onComplete
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.recovery.passphrase.title",
                    subtitleKey: "app.recovery.passphrase.subtitle",
                    symbolName: "text.word.spacing"
                )
                content
            }
        }
        .interactiveDismissDisabled()
        .task { await model.reveal() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .idle, .loading:
            AuthLoadingView(labelKey: "app.recovery.passphrase.loading")
        case let .failed(error):
            AuthErrorBanner(error: error) { Task { await model.reveal() } }
        case .empty:
            ContentUnavailableView(
                "app.recovery.passphrase.empty.title",
                systemImage: "tray",
                description: Text("app.recovery.passphrase.empty.description")
            )
        case .ready:
            stage
        }
    }

    @ViewBuilder
    private var stage: some View {
        switch model.stage {
        case .reveal: revealStage
        case .typeBack: typeBackStage
        case .completed: completedStage
        }
    }

    // MARK: Reveal

    private var revealStage: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
            shownOnceWarning
            RecoveryWordGrid(words: model.revealedWords)
            copyControls
            if let entropy = model.entropy {
                RecoveryEntropyMeter(estimate: entropy)
            }
            Button("app.recovery.passphrase.continue") { model.beginTypeBack() }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(model.revealedWords.isEmpty)
                .accessibilityLabel("app.recovery.passphrase.continue")
        }
    }

    /// The warning is above the words, not below them.
    ///
    /// A user who has already read the phrase and moved on will not scroll back
    /// for a caveat; the one chance to say "this is shown once" is before they
    /// have seen what it is about.
    private var shownOnceWarning: some View {
        Label {
            Text("app.recovery.passphrase.shown_once")
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill")
        }
        .foregroundStyle(.orange)
        .authCard()
        .accessibilityElement(children: .combine)
    }

    private var copyControls: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Button(
                model.hasCopied ? "app.recovery.passphrase.copied" : "app.recovery.passphrase.copy",
                systemImage: model.hasCopied ? "checkmark" : "doc.on.doc"
            ) {
                copySecret()
            }
            .capsuleGlassButtonStyle()
            .accessibilityLabel("app.recovery.passphrase.copy")

            Text("app.recovery.passphrase.copy_note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The one sanctioned plaintext egress: the pasteboard, because the user
    /// asked. Refusing would push them towards photographing the screen, which
    /// is strictly worse — ``SecretPasteboard`` makes the copy as short-lived
    /// and as un-recorded as each platform permits.
    private func copySecret() {
        guard let secret = model.secretForCopying() else { return }
        SecretPasteboard.copy(secret)
        model.markCopied()
    }

    // MARK: Type-back gate

    private var typeBackStage: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
            AuthSectionHeader(
                titleKey: "app.recovery.passphrase.gate.title",
                descriptionKey: "app.recovery.passphrase.gate.subtitle",
                symbolName: "checkmark.rectangle.stack"
            )
            ForEach(model.challenges) { challenge in
                challengeField(challenge)
            }
            remainingLine
            gateActions
            Text("app.recovery.passphrase.gate.no_skip")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func challengeField(_ challenge: TypeBackChallenge) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            HStack(spacing: CapsuleTheme.Spacing.xSmall) {
                Text("app.recovery.passphrase.gate.word_label")
                    .font(.headline)
                Text(verbatim: "\(challenge.displayPosition)")
                    .font(.headline.monospacedDigit())
            }
            .accessibilityElement(children: .combine)

            TextField(
                "app.recovery.passphrase.gate.word_label",
                text: answerBinding(for: challenge.wordIndex)
            )
            .textFieldStyle(.roundedBorder)
            .font(.body.monospaced())
            .autocorrectionDisabled()
            .textContentType(.none)
            .accessibilityLabel("app.recovery.passphrase.gate.word_label")
            .accessibilityValue(Text(verbatim: "\(challenge.displayPosition)"))

            if !challenge.typed.isEmpty {
                StatusChip(
                    titleKey: challenge.isVerified
                        ? "app.recovery.passphrase.gate.verified"
                        : "app.recovery.passphrase.gate.unverified",
                    symbolName: challenge.isVerified ? "checkmark.circle.fill" : "xmark.circle.fill",
                    tint: challenge.isVerified ? .green : .orange
                )
            }
        }
    }

    private var remainingLine: some View {
        HStack(spacing: CapsuleTheme.Spacing.xSmall) {
            Text("app.recovery.passphrase.gate.remaining")
                .font(.callout)
                .foregroundStyle(.secondary)
            Text(verbatim: "\(model.remainingChallengeCount)")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var gateActions: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Button("app.recovery.passphrase.gate.finish") {
                if model.complete() { answers.removeAll() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(!model.canComplete)
            .accessibilityLabel("app.recovery.passphrase.gate.finish")

            // Allowed only *before* the gate passes. The phrase is already on
            // this screen in this session, so refusing to re-show it would make
            // the user guess — the opposite of what the gate is for.
            Button("app.recovery.passphrase.gate.show_again") { model.returnToReveal() }
                .buttonStyle(.borderless)
                .accessibilityLabel("app.recovery.passphrase.gate.show_again")
        }
    }

    private func answerBinding(for index: Int) -> Binding<String> {
        Binding(
            get: { answers[index] ?? "" },
            set: { typed in
                answers[index] = typed
                model.submit(typed, forWordAt: index)
            }
        )
    }

    // MARK: Completed

    private var completedStage: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.recovery.passphrase.done.title",
                descriptionKey: "app.recovery.passphrase.done.description",
                symbolName: "checkmark.seal.fill"
            )
            Button("app.recovery.passphrase.done.action", action: onComplete)
                .capsuleGlassButtonStyle(prominent: true)
                .accessibilityLabel("app.recovery.passphrase.done.action")
        }
        .authCard()
    }
}

// MARK: - Previews

#Preview("Recovery passphrase — reveal") {
    RecoveryPassphraseView(recovery: AuthPreviewEnvironment.neverSignedIn.recovery)
}

#Preview("Recovery passphrase — rotate, dark") {
    RecoveryPassphraseView(
        recovery: AuthPreviewEnvironment.recoveryOverdue.recovery,
        source: .rotate
    )
    .preferredColorScheme(.dark)
}

#Preview("Recovery passphrase — offline") {
    RecoveryPassphraseView(recovery: AuthPreviewEnvironment.offline.recovery)
}
