import CapsuleDomain
import Foundation
import Observation

// MARK: - PasskeyEnrollViewModel

/// Drives passkey enrolment.
///
/// The screen's honest claim is narrow: a passkey replaces the *password* in
/// the login ceremony. It is not a second copy of the master key and it cannot
/// recover an account — that is the recovery secret's job, and conflating them
/// would leave a user believing a lost phone is survivable when it is not.
@MainActor
@Observable
public final class PasskeyEnrollViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var registration: PasskeyRegistration?
    public private(set) var isEnrolling = false
    /// Whether this platform has an authenticator at all. A device with none is
    /// told so, rather than shown a button that fails when tapped.
    public private(set) var isAvailable = false

    /// The label the user gives the credential, so a later revocation screen
    /// shows something they recognise.
    public var displayNameInput = ""

    private let secondFactor: any SecondFactorPort

    public init(secondFactor: any SecondFactorPort, defaultDisplayName: String = "") {
        self.secondFactor = secondFactor
        displayNameInput = defaultDisplayName
    }

    public var canEnroll: Bool {
        isAvailable && !isEnrolling && registration == nil
    }

    public func load() async {
        state = .loading
        isAvailable = await secondFactor.isPasskeyEnrollmentAvailable()
        state = isAvailable ? .ready : .empty
    }

    public func enroll() async {
        guard canEnroll else { return }
        isEnrolling = true
        state = .loading
        defer { isEnrolling = false }
        do {
            registration = try await secondFactor.enrollPasskey(displayName: displayNameInput)
            state = .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }
}
