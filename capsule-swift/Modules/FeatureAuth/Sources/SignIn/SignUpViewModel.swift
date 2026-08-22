import CapsuleDomain
import Foundation
import Observation

// MARK: - SignUpViewModel

/// Drives account creation on a server that runs its own credential ceremony.
///
/// What this screen creates is deliberately modest, and the copy says so: it
/// establishes the **account** and its server-side metadata only, and confers no
/// data access (*Device Enrollment — Account-creation auth*). The master key
/// that actually authorises decryption is minted by the first-device ceremony
/// immediately afterwards. A user who thinks this screen protected their photos
/// would be wrong about which secret matters.
@MainActor
@Observable
public final class SignUpViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var account: AccountSummary?
    public private(set) var isSubmitting = false

    public var handleInput = ""
    public var passwordInput = ""
    public var passwordConfirmationInput = ""

    /// The shortest account password the client will send.
    ///
    /// A **usability** floor, not a security one: this password guards the
    /// session, while the ≥128-bit recovery secret guards the data
    /// (*Backup & Recovery — Master-Key Escrow*). Pretending a password rule
    /// protects the library would misrepresent where the security actually
    /// lives.
    public static let minimumPasswordLength = 12

    private let credentials: any LocalCredentialPort

    public init(credentials: any LocalCredentialPort) {
        self.credentials = credentials
    }

    public var passwordsMatch: Bool {
        !passwordInput.isEmpty && passwordInput == passwordConfirmationInput
    }

    public var passwordIsLongEnough: Bool {
        passwordInput.count >= Self.minimumPasswordLength
    }

    public var canSubmit: Bool {
        !isSubmitting && handleInput.contains("@") && passwordsMatch && passwordIsLongEnough
    }

    /// Whether the handle is already taken, so the field can say so rather than
    /// a banner the user has to map back to a field.
    public var handleIsTaken: Bool {
        state.failure?.code == .authUserAlreadyExists
    }

    public var isRateLimited: Bool {
        state.failure?.code == .authRateLimited
    }

    public func createAccount() async {
        guard canSubmit else { return }
        let secret = RedactedSecret(passwordInput)
        defer {
            passwordInput = ""
            passwordConfirmationInput = ""
        }
        isSubmitting = true
        state = .loading
        defer { isSubmitting = false }
        do {
            account = try await credentials.createAccount(handle: handleInput, password: secret)
            state = .ready
        } catch {
            account = nil
            state = .failed(AuthPresentableError(error))
        }
    }
}
