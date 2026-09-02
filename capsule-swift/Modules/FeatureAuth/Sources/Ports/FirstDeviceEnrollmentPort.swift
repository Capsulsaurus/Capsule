import CapsuleDomain
import Foundation

// MARK: - EnrollmentStage

/// The six steps of first-device enrollment, in the order
/// *Device Enrollment — First-Device Enrollment* specifies them.
///
/// Ordered and closed because the ceremony is a **named** sequence, not a
/// percentage: "generating your device's keys" is something a user can wait
/// through and, if it fails, act on, whereas "47%" is something they can only
/// stare at. The rail renders one row per case.
public enum EnrollmentStage: String, Sendable, Hashable, CaseIterable {
    /// Draw the account master key from the OS CSPRNG, wrap it under the
    /// recovery passphrase, and escrow the wrapped blob.
    case masterKey = "master_key"
    /// Generate the user identity key pair; private halves wrapped under the
    /// master key.
    case userIdentityKey = "user_identity_key"
    /// Generate this device's signing and encryption keys, in the secure
    /// element where the platform has one.
    case deviceKeys = "device_keys"
    /// Upload the IK-signed device directory.
    case publishDirectory = "publish_directory"
    /// Establish the owner's default album, so there is a writable import
    /// destination from the first moment.
    case defaultAlbum = "default_album"
    /// Show the recovery passphrase, gated by the type-back check.
    case recoveryPassphrase = "recovery_passphrase"

    /// The catalog key for this stage's title.
    public var titleKey: String { "app.enrollment.stage.\(rawValue).title" }

    /// The catalog key for the one plain-language sentence under the title.
    public var explanationKey: String { "app.enrollment.stage.\(rawValue).explanation" }

    /// The SF Symbol for the rail. A symbol *and* a label: colour alone never
    /// carries the state.
    public var symbolName: String {
        switch self {
        case .masterKey: "key.fill"
        case .userIdentityKey: "person.badge.key.fill"
        case .deviceKeys: "lock.laptopcomputer"
        case .publishDirectory: "arrow.up.document.fill"
        case .defaultAlbum: "rectangle.stack.badge.plus"
        case .recoveryPassphrase: "text.word.spacing"
        }
    }
}

// MARK: - EnrollmentStageFailure

/// Why a stage stopped.
///
/// A typed failure rather than a bare ``CapsuleError`` because the two cases
/// that matter here have no error code and completely different recoveries:
/// hardware-key refusal is the documented "actionable error" of
/// *Device Enrollment — Failure Modes*, and it offers a **documented
/// deviation** rather than a dead end.
public enum EnrollmentStageFailure: Sendable, Equatable, Hashable {
    /// The secure element refused to generate a key. Rare, but it happens, and
    /// the ceremony must offer Retry or software keys rather than stopping.
    case hardwareKeyUnavailable
    /// The server said no, or could not be reached.
    case server(AuthPresentableError)
    /// The user abandoned the ceremony.
    case cancelled

    /// Whether continuing with software keys is the documented deviation for
    /// this failure.
    public var offersSoftwareKeyDeviation: Bool {
        self == .hardwareKeyUnavailable
    }
}

// MARK: - EnrollmentStageStatus

/// Where one stage stands.
///
/// ``deferred`` is not a softened failure — it is the documented outcome for
/// two specific stages. A directory upload that cannot reach the server leaves
/// the device "locally functional but invisible to other devices until the
/// upload succeeds", and a default album that fails to create is recreated
/// lazily from a master-key-derived id. Both explicitly **must not block
/// setup**, so they cannot be modelled as failures without the UI lying.
public enum EnrollmentStageStatus: Sendable, Equatable, Hashable {
    case pending
    case running
    case done
    /// Finished as far as the user is concerned; will complete in the
    /// background. Carries the catalog key explaining what is outstanding.
    case deferred(reasonKey: String)
    case failed(EnrollmentStageFailure)

    public var isTerminal: Bool {
        switch self {
        case .done, .deferred, .failed: true
        case .pending, .running: false
        }
    }
}

// MARK: - EnrollmentStageEvent

/// One stage transition.
public struct EnrollmentStageEvent: Sendable, Equatable, Hashable {
    public var stage: EnrollmentStage
    public var status: EnrollmentStageStatus

    public init(stage: EnrollmentStage, status: EnrollmentStageStatus) {
        self.stage = stage
        self.status = status
    }
}

// MARK: - HardwareKeyAvailability

/// Whether this device can put the classical half of its device keys inside a
/// secure element.
///
/// Shipping secure elements expose ECDSA/ECDH-P256 and hold no PQ keys, so even
/// the hardware-backed composition software-seals its PQ half — the honest
/// label is "hardware-backed where the platform allows", never "hardware keys".
public enum HardwareKeyAvailability: Sendable, Equatable, Hashable {
    /// A secure element is present and usable.
    case secureElement
    /// No secure element, or one that refused. Keys are generated in software.
    case softwareOnly
}

// MARK: - FirstDeviceEnrollmentPort

/// The first-device ceremony.
///
/// **Not yet in `CapsulePorts`.** `first_device_setup` lives in
/// `capsule-core::crypto::keys` (*Device Enrollment — Contract Skeleton*); this
/// protocol is the shape the screen needs from it and moves to `CapsulePorts`
/// with the FFI surface.
public protocol FirstDeviceEnrollmentPort: Sendable {
    /// What this device can do, asked **before** the ceremony starts so the
    /// rail can say "in this device's secure element" or "in software" up
    /// front rather than changing its story halfway through.
    func hardwareKeyAvailability() async -> HardwareKeyAvailability

    /// Run the ceremony, reporting each stage as it moves.
    ///
    /// - Parameter allowingSoftwareKeys: the documented deviation. `false` on
    ///   the first attempt; `true` only after the user has been told what they
    ///   are accepting and has chosen it.
    func run(allowingSoftwareKeys: Bool) -> AsyncStream<EnrollmentStageEvent>

    /// Abandon a ceremony in flight. No state is persisted; the user starts
    /// over.
    func cancel() async
}
