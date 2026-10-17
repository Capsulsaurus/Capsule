import CapsuleDomain
import Foundation
import Observation

// MARK: - TotpEnrollViewModel

/// Drives TOTP enrolment: mint a seed, show it for transcription, and arm it
/// only once the user has proved they transcribed it.
///
/// The seed and its `otpauth://` URI are ``RedactedSecret``s and stay that way.
/// They are rendered, and they are gone when the screen is: nothing writes them
/// to defaults, a file, or a log, because anything that captured either one
/// reproduces the second factor for whoever reads it.
///
/// The confirm-before-arm order is the same reasoning as the recovery
/// type-back gate: an enrolment that took effect the moment the seed appeared
/// would lock out any user whose authenticator app crashed mid-scan.
@MainActor
@Observable
public final class TotpEnrollViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var isConfirming = false
    public private(set) var isConfirmed = false
    /// Whether the manual seed is revealed under the QR code. Starts hidden so
    /// a shoulder-surfer needs a deliberate tap, not a glance.
    public private(set) var isSeedRevealed = false

    /// The six digits from the authenticator app.
    public var codeInput = ""

    private var enrollment: TotpEnrollment?
    private let secondFactor: any SecondFactorPort

    public init(secondFactor: any SecondFactorPort) {
        self.secondFactor = secondFactor
    }

    /// How many digits a TOTP code has. Used to enable Confirm, never to
    /// validate the code — only the server can do that.
    public static let codeLength = 6

    public var accountLabel: String { enrollment?.accountLabel ?? "" }
    public var issuer: String { enrollment?.issuer ?? "" }

    /// The seed, chunked, for the user to type into another authenticator.
    /// Empty until they ask for it.
    public var seedDisplay: String {
        guard isSeedRevealed, let enrollment else { return "" }
        return ChunkedCodeFormatter.chunked(enrollment.seed.reveal())
    }

    /// The payload the QR code encodes. A secret: it contains the seed.
    public func provisioningURIForQRCode() -> String? {
        enrollment?.provisioningURI.reveal()
    }

    public var canConfirm: Bool {
        enrollment != nil
            && !isConfirming
            && !isConfirmed
            && codeInput.count == Self.codeLength
    }

    public var isCodeRejected: Bool {
        state.failure?.code == .authInvalidCredentials
    }

    public var isRateLimited: Bool {
        state.failure?.code == .authRateLimited
    }

    public func begin() async {
        state = .loading
        do {
            enrollment = try await secondFactor.beginTotpEnrollment()
            state = .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    public func revealSeed() {
        isSeedRevealed = true
    }

    /// Confirm the transcription, then forget the typed code.
    public func confirm() async {
        guard canConfirm else { return }
        let code = codeInput
        defer { codeInput = "" }
        isConfirming = true
        state = .loading
        defer { isConfirming = false }
        do {
            try await secondFactor.confirmTotp(code: code)
            isConfirmed = true
            enrollment = nil
            isSeedRevealed = false
            state = .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }
}
