import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - LoginView

/// Sign-in against a server that runs its own credential ceremony, with the
/// identity-provider hand-off beside it when the deployment offers one.
///
/// Failures are surfaced by **code**, not by guesswork: `invalid_credentials`
/// and `rate_limited` are different conditions with different remedies, and the
/// catalog holds the wording for each. The screen never says which credential
/// was wrong, because the server deliberately does not say either — an error
/// that distinguished "no such user" from "wrong password" would be a free
/// account-enumeration oracle.
///
/// Entry point: ``init(credentials:auth:server:)``, needing
/// ``LocalCredentialPort``, ``AuthPort``, and the discovered ``ServerInfo``.
public struct LoginView: View {
    @State private var model: LoginViewModel

    public init(credentials: any LocalCredentialPort, auth: any AuthPort, server: ServerInfo) {
        _model = State(wrappedValue: LoginViewModel(
            credentials: credentials,
            auth: auth,
            server: server
        ))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.auth.login.title",
                    subtitleKey: "app.auth.login.subtitle",
                    symbolName: "person.badge.key.fill"
                )
                credentialFields
                if let failure = model.state.failure {
                    AuthErrorBanner(error: failure) {
                        Task { await model.signIn() }
                    }
                }
                if model.isSubmitting {
                    AuthLoadingView(labelKey: "app.auth.login.submitting")
                }
                actions
            }
        }
        .task { await model.load() }
    }

    private var credentialFields: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            LabeledField(labelKey: "app.auth.login.handle.label") {
                TextField("app.auth.login.handle.prompt", text: $model.handleInput)
                    .textContentType(.username)
                    .autocorrectionDisabled()
                    .accessibilityLabel("app.auth.login.handle.label")
            }
            LabeledField(labelKey: "app.auth.login.password.label") {
                SecureField("app.auth.login.password.label", text: $model.passwordInput)
                    .textContentType(.password)
                    .accessibilityLabel("app.auth.login.password.label")
            }
            LabeledField(
                labelKey: "app.auth.login.totp.label",
                footerKey: "app.auth.login.totp.footer"
            ) {
                TextField("app.auth.login.totp.label", text: $model.totpInput)
                    .textContentType(.oneTimeCode)
                    .accessibilityLabel("app.auth.login.totp.label")
            }
            if model.showsCredentialFailure {
                StatusChip(
                    titleKey: "app.auth.login.credentials_rejected",
                    symbolName: "xmark.circle.fill",
                    tint: .red
                )
            }
            if model.isRateLimited {
                StatusChip(
                    titleKey: "app.auth.login.rate_limited.hint",
                    symbolName: "clock.badge.exclamationmark.fill",
                    tint: .orange
                )
            }
        }
    }

    @ViewBuilder
    private var actions: some View {
        Button("app.auth.login.submit") {
            Task { await model.signIn() }
        }
        .capsuleGlassButtonStyle(prominent: true)
        .disabled(!model.canSubmit)
        .accessibilityLabel("app.auth.login.submit")

        if model.supportsPasskeys {
            Button("app.auth.login.passkey") {
                Task { await model.signInWithPasskey() }
            }
            .capsuleGlassButtonStyle()
            .accessibilityLabel("app.auth.login.passkey")
        }
        Button("app.auth.login.oidc") {
            Task { await model.signInWithIdentityProvider() }
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("app.auth.login.oidc")
    }
}

// MARK: - LabeledField

/// A labelled form field.
///
/// A visible label rather than a placeholder, because a placeholder disappears
/// the moment the user types and takes the field's meaning with it — and
/// because a placeholder is not reliably read as a label by assistive
/// technology.
struct LabeledField<Content: View>: View {
    let labelKey: LocalizedStringKey
    var footerKey: LocalizedStringKey?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(labelKey)
                .font(.headline)
            content
                .textFieldStyle(.roundedBorder)
            if let footerKey {
                Text(footerKey)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

// MARK: - Previews

#Preview("Login") {
    let world = AuthPreviewEnvironment.neverSignedIn
    return LoginView(credentials: world.credentials, auth: world.auth, server: world.server)
}

#Preview("Login — rejected credential") {
    let world = AuthPreviewEnvironment(
        scenario: .neverSignedIn,
        credentialBehaviour: PreviewCredentialBehaviour(rejectsPassword: true)
    )
    return LoginView(credentials: world.credentials, auth: world.auth, server: world.server)
}

#Preview("Login — rate limited, dark") {
    let world = AuthPreviewEnvironment(
        scenario: .neverSignedIn,
        credentialBehaviour: PreviewCredentialBehaviour(isRateLimited: true)
    )
    return LoginView(credentials: world.credentials, auth: world.auth, server: world.server)
        .preferredColorScheme(.dark)
}
