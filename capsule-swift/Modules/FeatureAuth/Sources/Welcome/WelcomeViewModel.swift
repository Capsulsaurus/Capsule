import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - WelcomeChoice

/// The two ways out of the first-run screen.
///
/// **Two paths, not a path and an escape hatch.** *Local Gallery — FR2* makes
/// never-signed-in a valid mode: a user who installs the app and never connects
/// a server gets import, organise, search, and export, in full. Sync is an
/// addition to a working product, not its precondition — so the two cases here
/// are peers, and the screen renders them as peers.
public enum WelcomeChoice: Sendable, Equatable, Hashable, CaseIterable {
    /// Use the app as a complete local photo library, with no server at all.
    case useWithoutAccount
    /// Connect to a Capsule server.
    case connectServer
}

// MARK: - WelcomeViewModel

/// Drives the first-run fork.
///
/// It holds a port only to answer one question: is there already a session on
/// this device? A user returning to a signed-in app must not be shown a
/// first-run screen, and a user whose session merely lapsed must be offered
/// re-authentication rather than a fresh start — *Authentication — Session
/// Expiry* is explicit that a lapsed session loses nothing locally.
@MainActor
@Observable
public final class WelcomeViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var choice: WelcomeChoice?
    /// The account already on this device, when there is one.
    public private(set) var existingAccount: AccountSummary?
    /// Whether the existing session lapsed and needs re-authentication.
    public private(set) var sessionHasExpired = false

    private let auth: any AuthPort

    public init(auth: any AuthPort) {
        self.auth = auth
    }

    /// Whether this really is a first run, or the app is merely showing the
    /// welcome route to someone who already has a session.
    public var isFirstRun: Bool {
        existingAccount == nil
    }

    public func load() async {
        state = .loading
        switch await auth.state() {
        case .signedOut:
            existingAccount = nil
            sessionHasExpired = false
        case let .signedIn(account), let .requiresLocalAuth(account):
            existingAccount = account
            sessionHasExpired = false
        case let .expired(account):
            existingAccount = account
            sessionHasExpired = true
        }
        state = .ready
    }

    /// Record the user's choice. The route table reads ``choice``; this type
    /// deliberately owns no navigation.
    public func choose(_ choice: WelcomeChoice) {
        self.choice = choice
    }
}
