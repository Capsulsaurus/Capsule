import CapsuleDomain
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - RecoveryVerificationPromptView

/// The periodic "do you still have your recovery phrase" check.
///
/// **It never blocks.** Every control on this screen is a way out: Not now is
/// always present, the snooze buttons are real, and nothing here gates sync,
/// unlock, or import. *Backup & Recovery — Recovery Verification Cadence* is
/// explicit that a UI which gated anything on this has misread it, so the screen
/// is built with no path that could.
///
/// What it *does* escalate is visibility, not consequence: 7 days → 90 → a
/// 180-day cap, at most three consecutive snoozes, and after that a persistent
/// badge that keeps saying so without ever standing in the way. Three failed
/// attempts turn the ask into an offer — the guided re-wrap, which replaces the
/// wrap around the **same** master key rather than re-encrypting anything.
///
/// Entry point: ``init(recovery:now:onDismiss:)``, needing ``RecoveryPort``.
public struct RecoveryVerificationPromptView: View {
    @State private var model: RecoveryVerificationViewModel
    @State private var isRewrapping = false
    private let recovery: any RecoveryPort
    private let onDismiss: () -> Void

    public init(
        recovery: any RecoveryPort,
        now: (@Sendable () -> CapsuleTimestamp)? = nil,
        onDismiss: @escaping () -> Void = {}
    ) {
        self.recovery = recovery
        self.onDismiss = onDismiss
        if let now {
            _model = State(wrappedValue: RecoveryVerificationViewModel(recovery: recovery, now: now))
        } else {
            _model = State(wrappedValue: RecoveryVerificationViewModel(recovery: recovery))
        }
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.recovery.verify.title",
                    subtitleKey: "app.recovery.verify.subtitle",
                    symbolName: "checkmark.shield"
                )
                content
            }
        }
        .task { await model.load() }
        .sheet(isPresented: $isRewrapping) {
            RecoveryPassphraseView(recovery: recovery, source: .rotate) {
                model.rearmAfterEnrollment()
                isRewrapping = false
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .idle, .loading:
            AuthLoadingView(labelKey: "app.recovery.verify.loading")
        case let .failed(error):
            AuthErrorBanner(error: error) { Task { await model.load() } }
        case .empty:
            ContentUnavailableView(
                "app.recovery.verify.unconfigured.title",
                systemImage: "shield.slash",
                description: Text("app.recovery.verify.unconfigured.description")
            )
        case .ready:
            armedContent
        }
    }

    @ViewBuilder
    private var armedContent: some View {
        if model.isArmed {
            cadence
            prompt
            outcome
            rewrapOffer
            footer
        } else {
            // A sponsored account holds no root of its own — every path routes
            // through the sponsor's — so the cadence never prompts one, and the
            // screen must not invent an ask for a user with nothing to verify.
            ContentUnavailableView(
                "app.recovery.verify.not_armed.title",
                systemImage: "person.2.badge.key",
                description: Text("app.recovery.verify.not_armed.description")
            )
        }
    }

    private var cadence: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            HStack(spacing: CapsuleTheme.Spacing.xSmall) {
                Text("app.recovery.verify.cadence")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text(verbatim: "\(model.currentIntervalDays)")
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)

            if model.isDue {
                StatusChip(
                    titleKey: "app.recovery.verify.due",
                    symbolName: "bell.badge.fill",
                    tint: .orange
                )
            }
            if model.showsPersistentBadge {
                StatusChip(
                    titleKey: "app.recovery.verify.badge.persistent",
                    symbolName: "exclamationmark.circle.fill",
                    tint: .orange
                )
                Text("app.recovery.verify.badge.description")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .authCard()
    }

    private var prompt: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            LabeledField(
                labelKey: "app.recovery.verify.field.label",
                footerKey: "app.recovery.verify.field.footer"
            ) {
                SecureField("app.recovery.verify.field.label", text: $model.passphraseInput)
                    .textContentType(.password)
                    .accessibilityLabel("app.recovery.verify.field.label")
            }
            if model.isVerifying {
                AuthLoadingView(labelKey: "app.recovery.verify.checking")
            }
            Button("app.recovery.verify.submit") { Task { await model.verify() } }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(model.isVerifying || model.passphraseInput.isEmpty)
                .accessibilityLabel("app.recovery.verify.submit")
        }
    }

    @ViewBuilder
    private var outcome: some View {
        if let outcome = model.lastOutcome {
            outcomeChip(outcome)
        }
    }

    @ViewBuilder
    private func outcomeChip(_ outcome: RecoveryVerificationOutcome) -> some View {
        switch outcome {
        case .verified:
            StatusChip(
                titleKey: "app.recovery.verify.outcome.verified",
                symbolName: "checkmark.seal.fill",
                tint: .green
            )
        case .mismatch:
            StatusChip(
                titleKey: "app.recovery.verify.outcome.mismatch",
                symbolName: "xmark.circle.fill",
                tint: .red
            )
        case .inconclusive:
            // Explicitly not a failure. The escrow could not be read, which is a
            // network problem, and recording it against the user would punish
            // them for it.
            StatusChip(
                titleKey: "app.recovery.verify.outcome.inconclusive",
                symbolName: "questionmark.circle.fill",
                tint: .orange
            )
        }
    }

    @ViewBuilder
    private var rewrapOffer: some View {
        if model.offersGuidedRewrap {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
                AuthSectionHeader(
                    titleKey: "app.recovery.verify.rewrap.title",
                    descriptionKey: "app.recovery.verify.rewrap.description",
                    symbolName: "arrow.triangle.2.circlepath"
                )
                Button("app.recovery.verify.rewrap.action") { isRewrapping = true }
                    .capsuleGlassButtonStyle(prominent: true)
                    .accessibilityLabel("app.recovery.verify.rewrap.action")
            }
            .authCard()
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            if model.canSnooze {
                snoozeButtons
            }
            Button("app.recovery.verify.lost") { model.declareSecretLost() }
                .buttonStyle(.borderless)
                .accessibilityLabel("app.recovery.verify.lost")
            Button("app.common.not_now", action: onDismiss)
                .buttonStyle(.borderless)
                .accessibilityLabel("app.common.not_now")
            Text("app.recovery.verify.never_blocks")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var snoozeButtons: some View {
        HStack(spacing: CapsuleTheme.Spacing.small) {
            ForEach(RecoverySnooze.allCases, id: \.self) { snooze in
                Button(LocalizedStringKey(snooze.titleKey)) {
                    Task { await model.snooze(snooze) }
                }
                .buttonStyle(.bordered)
                .accessibilityLabel(LocalizedStringKey(snooze.titleKey))
            }
        }
    }
}

// MARK: - Previews

#Preview("Recovery verification — due") {
    let world = AuthPreviewEnvironment.recoveryOverdue
    return RecoveryVerificationPromptView(recovery: world.recovery, now: world.now)
}

#Preview("Recovery verification — healthy, dark") {
    let world = AuthPreviewEnvironment.healthy
    return RecoveryVerificationPromptView(recovery: world.recovery, now: world.now)
        .preferredColorScheme(.dark)
}

#Preview("Recovery verification — offline") {
    let world = AuthPreviewEnvironment.offline
    return RecoveryVerificationPromptView(recovery: world.recovery, now: world.now)
}
