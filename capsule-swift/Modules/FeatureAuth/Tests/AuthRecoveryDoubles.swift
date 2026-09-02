import CapsuleDomain
import CapsulePorts
import FeatureAuth
import Foundation

// MARK: - StubRecoveryPort

/// A ``RecoveryPort`` that mints a known phrase.
///
/// The type-back gate can only be tested against a secret the test knows the
/// words of, and the view model deliberately never hands the plaintext out
/// except through the one greppable egress. So the double is where the phrase
/// comes from.
actor StubRecoveryPort: RecoveryPort {
    /// Twelve distinct words — a phrase that clears the 128-bit floor, and one
    /// in which a wrong answer cannot accidentally be right.
    static let phrase = "harbor-lantern-quartz-meadow-cobalt-thistle-ember-willow-granite-cinder-marlin-juniper"
    /// The phrase a guided re-wrap mints instead.
    static let rotatedPhrase = "beacon-pumice-fathom-cedar-onyx-plover-tundra-basalt-verdant-quill-saffron-halyard"

    private let setUpFailure: CapsuleError?
    private let restoreSecret: String?
    private let hasServerEscrow: Bool
    private let verification: RecoveryVerificationState
    private let mintedPhrase: String
    private(set) var verifiedPassphrases: [String] = []
    private(set) var snoozedUntil: [CapsuleTimestamp] = []

    init(
        setUpFailure: CapsuleError? = nil,
        restoreSecret: String? = nil,
        hasServerEscrow: Bool = true,
        verification: RecoveryVerificationState = RecoveryVerificationState(),
        mintedPhrase: String = StubRecoveryPort.phrase
    ) {
        self.setUpFailure = setUpFailure
        self.restoreSecret = restoreSecret
        self.hasServerEscrow = hasServerEscrow
        self.verification = verification
        self.mintedPhrase = mintedPhrase
    }

    func summary() async throws -> RecoveryEscrowSummary {
        RecoveryEscrowSummary(
            hasServerEscrow: hasServerEscrow,
            escrowUpdatedAt: AuthInstant.days(-40),
            verification: verification
        )
    }

    func setUpRecovery() async throws -> String {
        if let setUpFailure { throw setUpFailure }
        return mintedPhrase
    }

    func verify(passphrase: String) async throws -> RecoveryVerificationOutcome {
        verifiedPassphrases.append(passphrase)
        return passphrase == mintedPhrase ? .verified : .mismatch
    }

    func snoozeVerification(until: CapsuleTimestamp) async throws {
        snoozedUntil.append(until)
    }

    func rotateRecoverySecret() async throws -> String {
        if let setUpFailure { throw setUpFailure }
        return Self.rotatedPhrase
    }

    func restore(usingRecoverySecret secret: String) async throws -> AccountSummary {
        guard secret == restoreSecret else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: the secret does not unwrap")
        }
        return AccountSummary(
            handle: "avery@capsule.example",
            userID: "user-1",
            homeServer: "capsule.example",
            accountType: .registered
        )
    }
}

// MARK: - StubRestorePort

/// A ``RestorePort`` implementing the documented mode ladder and a 2-of-3
/// Shamir threshold.
///
/// It refuses a commit without a dry run and refuses a wrong phrase, because
/// the port checking both is the contract the view model's gate is layered on
/// top of — a double that accepted anything would let a UI-only gate pass for a
/// real one.
actor StubRestorePort: RestorePort {
    /// The reconstructed secret, so a paired ``StubRecoveryPort`` can accept it.
    static let reconstructedSecret = "granite-willow-ember-juniper-marlin-cinder"
    /// The threshold: any two live shares reconstruct, one alone does not.
    static let threshold = 2

    private let ledgerIsComplete: Bool
    private let signatureChainIsIntact: Bool
    private var hasRunDryRun = false
    private(set) var commitAttempts: [String] = []

    init(ledgerIsComplete: Bool = true, signatureChainIsIntact: Bool = true) {
        self.ledgerIsComplete = ledgerIsComplete
        self.signatureChainIsIntact = signatureChainIsIntact
    }

    func preview(artifact _: URL) async throws -> RestorePreview {
        RestorePreview(
            assetCount: 12480,
            totalBytes: 214748364800,
            exportedAt: AuthInstant.days(-181),
            exporterModel: "Mac16,7",
            artifactVersion: 1
        )
    }

    func dryRun(artifact _: URL) async throws -> RestoreDiff {
        hasRunDryRun = true
        return diff
    }

    func commit(artifact _: URL, confirmationPhrase: String) async throws -> RestoreDiff {
        commitAttempts.append(confirmationPhrase)
        guard hasRunDryRun else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: dry run has not been run")
        }
        guard confirmationPhrase == "RESTORE" else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: confirmation phrase mismatch")
        }
        guard diff.isCommittable else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: artifact failed verification")
        }
        return diff
    }

    func shamirShares() async throws -> [ShamirShareSummary] {
        [
            ShamirShareSummary(id: "share-1", label: "Safe deposit box", issuedAt: AuthInstant.days(-400)),
            ShamirShareSummary(id: "share-2", label: "Password manager", issuedAt: AuthInstant.days(-400)),
            ShamirShareSummary(
                id: "share-3",
                label: "Sister's house",
                issuedAt: AuthInstant.days(-400),
                isInvalidated: true
            ),
        ]
    }

    func reconstructSecret(fromShareIDs ids: [String]) async throws -> RedactedSecret {
        let live = try await shamirShares().filter { !$0.isInvalidated }.map(\.id)
        let usable = Set(ids).intersection(live)
        guard usable.count >= Self.threshold else {
            throw CapsuleError(code: .escrowMalformed, detail: "stub: quorum not met")
        }
        return RedactedSecret(Self.reconstructedSecret)
    }

    private var diff: RestoreDiff {
        RestoreDiff(
            addedCount: 11902,
            alreadyPresentCount: 502,
            conflictingCount: 61,
            supersededByLocalCount: 15,
            amkLedgerIsComplete: ledgerIsComplete,
            signatureChainIsIntact: signatureChainIsIntact
        )
    }
}

// MARK: - StubSecondFactorPort

/// A ``SecondFactorPort`` with each documented outcome asked for by name.
///
/// A cancelled platform ceremony is modelled as a thrown error rather than a
/// quiet `nil`, because that is what `ASAuthorizationController` and
/// `LAContext` actually do — and the screen's obligation is not to report it as
/// an enrolment.
actor StubSecondFactorPort: SecondFactorPort {
    /// The only code the stub accepts.
    static let acceptedCode = "123456"

    private let passkeysAvailable: Bool
    private let passkeyFailure: CapsuleError?
    private let beginFailure: CapsuleError?
    private let confirmFailure: CapsuleError?
    private(set) var enrolledDisplayNames: [String] = []
    private(set) var confirmedCodes: [String] = []

    init(
        passkeysAvailable: Bool = true,
        passkeyFailure: CapsuleError? = nil,
        beginFailure: CapsuleError? = nil,
        confirmFailure: CapsuleError? = nil
    ) {
        self.passkeysAvailable = passkeysAvailable
        self.passkeyFailure = passkeyFailure
        self.beginFailure = beginFailure
        self.confirmFailure = confirmFailure
    }

    func isPasskeyEnrollmentAvailable() async -> Bool { passkeysAvailable }

    func enrollPasskey(displayName: String) async throws -> PasskeyRegistration {
        enrolledDisplayNames.append(displayName)
        if let passkeyFailure { throw passkeyFailure }
        return PasskeyRegistration(
            id: "credential-1",
            authenticatorLabel: displayName.isEmpty ? "iCloud Keychain" : displayName,
            createdAt: AuthInstant.reference
        )
    }

    func beginTotpEnrollment() async throws -> TotpEnrollment {
        if let beginFailure { throw beginFailure }
        return TotpEnrollment(
            seed: RedactedSecret("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"),
            provisioningURI: RedactedSecret("otpauth://totp/Capsule:avery?secret=JBSWY3DPEHPK3PXP"),
            accountLabel: "avery@capsule.example",
            issuer: "Capsule"
        )
    }

    func confirmTotp(code: String) async throws {
        confirmedCodes.append(code)
        if let confirmFailure { throw confirmFailure }
        guard code == Self.acceptedCode else {
            throw CapsuleError(code: .authInvalidCredentials, detail: "stub: wrong code")
        }
    }
}
