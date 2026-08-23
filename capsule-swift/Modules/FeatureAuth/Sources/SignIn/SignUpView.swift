import CapsuleUI
import SwiftUI

// MARK: - SignUpView

/// Account creation on a server that runs its own credential ceremony.
///
/// The note under the form is not marketing copy — it is the correction to the
/// mental model this screen would otherwise create. Creating an account
/// establishes the account and its server-side metadata **only**; it confers no
/// data access. The key that authorises decryption is minted on this device in
/// the next step and the server never sees it, which is why a strong password
/// here is a courtesy and a strong *recovery phrase* there is the actual
/// defence.
///
/// Entry point: ``init(credentials:)``, needing ``LocalCredentialPort``.
public struct SignUpView: View {
    @State private var model: SignUpViewModel

    public init(credentials: any LocalCredentialPort) {
        _model = State(wrappedValue: SignUpViewModel(credentials: credentials))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.auth.signup.title",
                    subtitleKey: "app.auth.signup.subtitle",
                    symbolName: "person.crop.circle.badge.plus"
                )
                fields
                if let failure = model.state.failure, !model.handleIsTaken {
                    AuthErrorBanner(error: failure) {
                        Task { await model.createAccount() }
                    }
                }
                if model.isSubmitting {
                    AuthLoadingView(labelKey: "app.auth.signup.submitting")
                }
                Button("app.auth.signup.submit") {
                    Task { await model.createAccount() }
                }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(!model.canSubmit)
                .accessibilityLabel("app.auth.signup.submit")

                Text("app.auth.signup.scope_note")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var fields: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            LabeledField(labelKey: "app.auth.signup.handle.label") {
                TextField("app.auth.signup.handle.prompt", text: $model.handleInput)
                    .textContentType(.username)
                    .autocorrectionDisabled()
                    .accessibilityLabel("app.auth.signup.handle.label")
            }
            if model.handleIsTaken {
                StatusChip(
                    titleKey: "app.auth.signup.handle.taken",
                    symbolName: "exclamationmark.circle.fill",
                    tint: .orange
                )
            }
            LabeledField(
                labelKey: "app.auth.signup.password.label",
                footerKey: "app.auth.signup.password.requirement"
            ) {
                SecureField("app.auth.signup.password.label", text: $model.passwordInput)
                    .textContentType(.newPassword)
                    .accessibilityLabel("app.auth.signup.password.label")
            }
            LabeledField(labelKey: "app.auth.signup.password.confirm") {
                SecureField(
                    "app.auth.signup.password.confirm",
                    text: $model.passwordConfirmationInput
                )
                .textContentType(.newPassword)
                .accessibilityLabel("app.auth.signup.password.confirm")
            }
            if !model.passwordConfirmationInput.isEmpty, !model.passwordsMatch {
                StatusChip(
                    titleKey: "app.auth.signup.password.mismatch",
                    symbolName: "exclamationmark.circle.fill",
                    tint: .orange
                )
            }
        }
    }
}

// MARK: - Previews

#Preview("Sign up") {
    SignUpView(credentials: AuthPreviewEnvironment.neverSignedIn.credentials)
}

#Preview("Sign up — handle taken, dark") {
    let world = AuthPreviewEnvironment(
        scenario: .neverSignedIn,
        credentialBehaviour: PreviewCredentialBehaviour(handleIsTaken: true)
    )
    return SignUpView(credentials: world.credentials)
        .preferredColorScheme(.dark)
}
