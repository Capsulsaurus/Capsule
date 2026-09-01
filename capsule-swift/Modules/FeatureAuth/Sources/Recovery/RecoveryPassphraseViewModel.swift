import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - RecoverySecretSource

/// Where the secret this screen shows comes from.
///
/// Both paths end in the same type-back gate, which is not an accident:
/// *Backup & Recovery — On Repeated Failure* specifies that the guided re-wrap
/// "re-runs the setup-style type-back gate", so the flow is one screen with two
/// entry points rather than two screens that drift apart.
public enum RecoverySecretSource: Sendable, Equatable, Hashable {
    /// First-device setup: mint the secret and escrow the wrapped master key.
    case setUp
    /// Guided re-wrap: mint a fresh secret and re-wrap the **same** master key.
    case rotate
}

// MARK: - TypeBackChallenge

/// One word the user must type back.
///
/// Holds the *position*, never the word: the answer is checked against the
/// secret through ``RedactedSecret/matches(_:atWordIndex:)``, so the expected
/// value never gets copied into a second place that could outlive it.
public struct TypeBackChallenge: Sendable, Equatable, Hashable, Identifiable {
    /// Zero-based index into the passphrase.
    public var wordIndex: Int
    /// What the user has typed so far. Cleared when the flow completes.
    public var typed: String
    /// Whether the typed value matched.
    public var isVerified: Bool

    public var id: Int { wordIndex }

    /// The 1-based position a human sees.
    public var displayPosition: Int { wordIndex + 1 }

    public init(wordIndex: Int, typed: String = "", isVerified: Bool = false) {
        self.wordIndex = wordIndex
        self.typed = typed
        self.isVerified = isVerified
    }
}

// MARK: - RecoveryPassphraseViewModel

/// Drives the two halves of the recovery-passphrase flow: reveal, then the
/// type-back gate.
///
/// The rule the whole type exists to hold: **the secret is never persisted**.
/// It lives in one ``RedactedSecret`` for as long as the screen is on-screen and
/// is dropped the moment the gate passes. There is no `UserDefaults` write, no
/// keychain write, no file write, and no `Codable` conformance anywhere on the
/// path — because *Backup & Recovery — Local Verification* only works if the
/// client genuinely cannot answer the later verification prompt on the user's
/// behalf.
///
/// The second rule: **there is no skip**. Deliberately no `skip()`, no
/// `dismiss()`, no `continueAnyway()`. *Device Enrollment* step 6 gates setup on
/// the type-back precisely so the user records the phrase rather than dismissing
/// the screen, and a method that bypassed it would quietly undo that.
@MainActor
@Observable
public final class RecoveryPassphraseViewModel {
    /// Which half of the flow is showing.
    public enum Stage: Sendable, Equatable, Hashable {
        case reveal
        case typeBack
        case completed
    }

    public private(set) var state: ScreenState = .idle
    public private(set) var stage: Stage = .reveal
    public private(set) var challenges: [TypeBackChallenge] = []
    public private(set) var entropy: RecoveryEntropyEstimate?
    /// Whether the user has used Copy. Tracked only to soften the copy on the
    /// button; never written anywhere.
    public private(set) var hasCopied = false
    /// How many words the secret has, for a length hint before it is revealed.
    public private(set) var wordCount = 0

    /// How many positions the gate asks for.
    public static let challengeCount = 3

    private var secret: RedactedSecret?
    private let recovery: any RecoveryPort
    private let source: RecoverySecretSource
    private let policy: RecoveryEntropyPolicy

    public init(
        recovery: any RecoveryPort,
        source: RecoverySecretSource = .setUp,
        entropyPolicy: RecoveryEntropyPolicy = .bip39
    ) {
        self.recovery = recovery
        self.source = source
        policy = entropyPolicy
    }

    // MARK: Reveal

    /// The words, for the grid.
    ///
    /// Computed on demand rather than stored, so there is exactly one copy of
    /// the plaintext and clearing ``secret`` clears everything.
    public var revealedWords: [String] {
        secret?.words ?? []
    }

    /// Whether the meter's verdict should be shown as a problem.
    ///
    /// A generator that produced a below-floor secret is a **defect in this
    /// build**, not a user error — the screen says so rather than nagging the
    /// user to pick something stronger, because the user did not pick anything.
    public var isBelowEntropyFloor: Bool {
        guard let entropy else { return false }
        return !entropy.meetsFloor
    }

    /// Mint the secret and show it.
    public func reveal() async {
        state = .loading
        do {
            let plaintext = switch source {
            case .setUp: try await recovery.setUpRecovery()
            case .rotate: try await recovery.rotateRecoverySecret()
            }
            let minted = RedactedSecret(plaintext)
            secret = minted
            wordCount = minted.words.count
            entropy = RecoveryEntropy.estimate(wordCount: minted.words.count, policy: policy)
            stage = .reveal
            state = .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Note that the user copied the phrase. Records a `Bool` and nothing else.
    public func markCopied() {
        hasCopied = true
    }

    /// The plaintext, for the one sanctioned egress: putting it on the
    /// pasteboard because the user asked.
    ///
    /// Named to be greppable. Every other caller must use ``revealedWords``.
    public func secretForCopying() -> String? {
        secret?.reveal()
    }

    // MARK: Type-back gate

    /// Move to the gate, choosing the positions to ask for.
    public func beginTypeBack() {
        guard let secret else { return }
        var generator = SystemRandomNumberGenerator()
        let positions = Self.challengePositions(
            wordCount: secret.words.count,
            count: Self.challengeCount,
            using: &generator
        )
        challenges = positions.map { TypeBackChallenge(wordIndex: $0) }
        stage = .typeBack
    }

    /// Go back to the phrase. Allowed **before** the gate passes, because the
    /// phrase is already on this screen in this session and refusing to re-show
    /// it would only make the user guess — which is the opposite of what the
    /// gate is for.
    public func returnToReveal() {
        guard stage == .typeBack else { return }
        stage = .reveal
    }

    /// Record what the user typed for one challenge and check it.
    public func submit(_ typed: String, forWordAt index: Int) {
        guard let secret, let position = challenges.firstIndex(where: { $0.wordIndex == index }) else {
            return
        }
        challenges[position].typed = typed
        challenges[position].isVerified = secret.matches(typed, atWordIndex: index)
    }

    /// Whether every challenge has been answered correctly. The **only** way
    /// past this screen.
    public var canComplete: Bool {
        !challenges.isEmpty && challenges.allSatisfy(\.isVerified)
    }

    /// How many challenges are still unanswered, for the progress line.
    public var remainingChallengeCount: Int {
        challenges.count { !$0.isVerified }
    }

    /// Finish, dropping the secret.
    ///
    /// Returns `false` when the gate has not passed, so a caller cannot
    /// complete the flow by calling this at the wrong moment.
    @discardableResult
    public func complete() -> Bool {
        guard canComplete else { return false }
        secret = nil
        challenges = challenges.map { TypeBackChallenge(wordIndex: $0.wordIndex, isVerified: true) }
        stage = .completed
        return true
    }

    /// Choose distinct word positions to ask about.
    ///
    /// Static and pure so the selection is testable without a port: a gate that
    /// asked for the same position twice, or for a position past the end of the
    /// phrase, would be trivially passable.
    public static func challengePositions(
        wordCount: Int,
        count: Int,
        using generator: inout some RandomNumberGenerator
    ) -> [Int] {
        guard wordCount > 0 else { return [] }
        return Array(0 ..< wordCount).shuffled(using: &generator).prefix(min(count, wordCount)).sorted()
    }
}
