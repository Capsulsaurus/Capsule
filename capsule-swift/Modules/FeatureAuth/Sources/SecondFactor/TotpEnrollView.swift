import CapsuleUI
import SwiftUI

// MARK: - TotpEnrollView

/// Enrolling a TOTP authenticator.
///
/// **Nothing is armed until the user proves they transcribed the seed.** An
/// enrolment that took effect the moment the QR appeared would lock out anyone
/// whose authenticator app crashed mid-scan — the same reasoning as the recovery
/// type-back gate, and the reason Confirm is a step rather than a courtesy.
///
/// The seed is a secret in both of its forms, and both are drawn and nothing
/// else: the QR *contains* the seed, so it is no less sensitive than the letters
/// under it. The manual form starts hidden behind a disclosure, so reading it
/// over someone's shoulder needs them to have deliberately opened it rather than
/// merely to have glanced at the screen while it was open for scanning.
///
/// Entry point: ``init(secondFactor:)``, needing ``SecondFactorPort``.
public struct TotpEnrollView: View {
    @State private var model: TotpEnrollViewModel
    @State private var isManualEntryExpanded = false

    public init(secondFactor: any SecondFactorPort) {
        _model = State(wrappedValue: TotpEnrollViewModel(secondFactor: secondFactor))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.totp.title",
                    subtitleKey: "app.totp.subtitle",
                    symbolName: "123.rectangle.fill"
                )
                content
            }
        }
        .task { await model.begin() }
    }

    @ViewBuilder
    private var content: some View {
        if model.isConfirmed {
            confirmed
        } else {
            enrolment
        }
    }

    @ViewBuilder
    private var enrolment: some View {
        switch model.state {
        case .idle, .loading:
            AuthLoadingView(labelKey: "app.totp.loading")
        case .empty:
            ContentUnavailableView(
                "app.totp.unavailable.title",
                systemImage: "tray",
                description: Text("app.totp.unavailable.description")
            )
        case .ready, .failed:
            seedSection
            confirmSection
        }
    }

    // MARK: Seed

    @ViewBuilder
    private var seedSection: some View {
        if let uri = model.provisioningURIForQRCode() {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
                SecretQRCodeView(payload: uri)
                AuthLabeledValue(labelKey: "app.totp.account", value: model.accountLabel)
                AuthLabeledValue(labelKey: "app.totp.issuer", value: model.issuer)
                manualEntry
            }
            .authCard()
        }
    }

    private var manualEntry: some View {
        DisclosureGroup(isExpanded: $isManualEntryExpanded) {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
                AuthCodeValue(labelKey: "app.totp.manual.seed", code: model.seedDisplay)
                Text("app.totp.manual.note")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.top, CapsuleTheme.Spacing.small)
        } label: {
            Text("app.totp.manual.title")
                .font(.headline)
        }
        .onChange(of: isManualEntryExpanded) { _, expanded in
            if expanded { model.revealSeed() }
        }
        .accessibilityLabel("app.totp.manual.title")
    }

    // MARK: Confirm

    private var confirmSection: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.totp.confirm.title",
                descriptionKey: "app.totp.confirm.description",
                symbolName: "checkmark.rectangle"
            )
            codeField
            refusals
            if model.isConfirming {
                AuthLoadingView(labelKey: "app.totp.confirming")
            }
            Button("app.totp.confirm.action") { Task { await model.confirm() } }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(!model.canConfirm)
                .accessibilityLabel("app.totp.confirm.action")
        }
        .authCard()
    }

    private var codeField: some View {
        LabeledField(labelKey: "app.totp.code.label", footerKey: "app.totp.code.footer") {
            TextField("app.totp.code.label", text: codeBinding)
                .font(.title3.monospacedDigit())
                .textContentType(.oneTimeCode)
                .authNumericField()
                .accessibilityLabel("app.totp.code.label")
        }
    }

    /// Digits only, six at most.
    ///
    /// Filtered here rather than trusted to the keyboard: a hardware keyboard, a
    /// paste, or a dictation pass can put anything at all in the field, and the
    /// number pad is an affordance rather than a constraint. It is *not*
    /// validation — only the server can say whether a code is right.
    private var codeBinding: Binding<String> {
        Binding(
            get: { model.codeInput },
            set: { typed in
                model.codeInput = String(typed.filter(\.isNumber).prefix(TotpEnrollViewModel.codeLength))
            }
        )
    }

    @ViewBuilder
    private var refusals: some View {
        if model.isCodeRejected {
            StatusChip(
                titleKey: "app.totp.code.rejected",
                symbolName: "xmark.circle.fill",
                tint: .red
            )
        }
        if model.isRateLimited {
            StatusChip(
                titleKey: "app.totp.code.rate_limited",
                symbolName: "clock.badge.exclamationmark.fill",
                tint: .orange
            )
        }
        if let failure = model.state.failure, !model.isCodeRejected, !model.isRateLimited {
            AuthErrorBanner(error: failure) { Task { await model.begin() } }
        }
    }

    // MARK: Confirmed

    private var confirmed: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.totp.done.title",
                descriptionKey: "app.totp.done.description",
                symbolName: "checkmark.seal.fill"
            )
        }
        .authCard()
    }
}

// MARK: - Previews

#Preview("TOTP enrolment") {
    TotpEnrollView(secondFactor: AuthPreviewEnvironment.healthy.secondFactor)
}

#Preview("TOTP enrolment — dark") {
    TotpEnrollView(secondFactor: AuthPreviewEnvironment.healthy.secondFactor)
        .preferredColorScheme(.dark)
}

#Preview("TOTP enrolment — offline") {
    TotpEnrollView(secondFactor: AuthPreviewEnvironment.offline.secondFactor)
}
