import Foundation

// MARK: - PlatformTag

/// The closed platform enum, shared by device cohorts and import scopes
/// (*Authentication — Device Cohorts*).
public enum PlatformTag: ClosedWireEnum {
    case ios
    case android
    case macos
    case windows
    case linux
    case unknown(String)

    public static let knownCases: [PlatformTag] = [.ios, .android, .macos, .windows, .linux]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .ios: "ios"
        case .android: "android"
        case .macos: "macos"
        case .windows: "windows"
        case .linux: "linux"
        case let .unknown(raw): raw
        }
    }

    /// Whether a device cohort on this platform survives a factory reset.
    ///
    /// Only macOS does. A reset destroys every app-accessible identifier on iOS
    /// and Android by OS design, and post-reset attestation yields verdicts, not
    /// identifiers. The honest promise is reinstall-stable everywhere,
    /// reset-stable only where the OS allows — and the UI says so rather than
    /// pretending otherwise.
    public var cohortSurvivesFactoryReset: Bool {
        self == .macos
    }
}

// MARK: - AccountType

/// What kind of account this is (*Authentication — Account Types*).
public enum AccountType: ClosedWireEnum {
    /// Its own identity and its own master key.
    case registered
    /// Encrypted under keys derived from a sponsor's master key; holds no root
    /// of its own, which is why the recovery-verification cadence never prompts
    /// one.
    case sponsored
    case unknown(String)

    public static let knownCases: [AccountType] = [.registered, .sponsored]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .registered: "registered"
        case .sponsored: "sponsored"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - AccountSummary

/// The signed-in account, as the UI needs it.
///
/// No token, no key, no key fingerprint. The SDK owns every secret; this layer
/// carries only what a settings screen displays and what a port needs to scope
/// a request.
public struct AccountSummary: Sendable, Equatable, Identifiable, Hashable {
    /// The `user@server.tld` handle.
    public var handle: String
    /// The user's opaque id on their home server.
    public var userID: String
    /// The display name, when set. Never fabricated from the handle.
    public var displayName: String?
    /// The home server's origin.
    public var homeServer: String
    public var accountType: AccountType

    public var id: String { userID }

    public init(
        handle: String,
        userID: String,
        displayName: String? = nil,
        homeServer: String,
        accountType: AccountType
    ) {
        self.handle = handle
        self.userID = userID
        self.displayName = displayName
        self.homeServer = homeServer
        self.accountType = accountType
    }
}

// MARK: - DeviceRecord

/// One enrolled device in the account's device directory.
///
/// A revoked device is **marked, never deleted**: its public half is retained
/// forever so every record it ever signed stays verifiable. A key-holding
/// attacker can append forward but cannot rewrite the past, and that property
/// depends on this row surviving revocation.
public struct DeviceRecord: Sendable, Equatable, Identifiable, Hashable {
    public var id: DeviceID
    /// The device's self-reported model.
    public var model: String
    public var platform: PlatformTag
    public var firstSeen: CapsuleTimestamp
    public var lastSeen: CapsuleTimestamp
    /// The advisory cohort hash grouping this device's successive enrollments.
    ///
    /// **Advisory only, structurally**: client-asserted and unverifiable, so no
    /// authorization or capability decision may read it. A garbage or absent
    /// value must behave identically to a valid one — otherwise it becomes
    /// spoofable attack surface.
    public var cohortHash: String?
    /// Whether this is the device the app is running on.
    public var isCurrent: Bool
    /// When the device was revoked, if it was.
    public var revokedAt: CapsuleTimestamp?

    public init(
        id: DeviceID,
        model: String,
        platform: PlatformTag,
        firstSeen: CapsuleTimestamp,
        lastSeen: CapsuleTimestamp,
        cohortHash: String? = nil,
        isCurrent: Bool = false,
        revokedAt: CapsuleTimestamp? = nil
    ) {
        self.id = id
        self.model = model
        self.platform = platform
        self.firstSeen = firstSeen
        self.lastSeen = lastSeen
        self.cohortHash = cohortHash
        self.isCurrent = isCurrent
        self.revokedAt = revokedAt
    }

    /// Whether the device can still author writes.
    public var isActive: Bool {
        revokedAt == nil
    }
}

// MARK: - SessionRecord

/// One row of the session ledger (*Authentication — Explicit Revocation*).
///
/// Carries no token. Revoking a *single* session is authenticated by any active
/// session token; revoking **all** sessions requires proof of master-key
/// possession — deliberately asymmetric, so an attacker holding a stolen token
/// can revoke only that session and cannot escalate to locking the legitimate
/// user out of every device.
public struct SessionRecord: Sendable, Equatable, Identifiable, Hashable {
    public var id: SessionID
    /// The device the session was created on, when known.
    public var deviceID: DeviceID?
    /// The advisory cohort the session belongs to. Sessions are grouped by this
    /// in the ledger so one physical phone's successive reinstalls read as one
    /// device rather than several strangers.
    public var cohortHash: String?
    public var createdAt: CapsuleTimestamp
    /// Last successful access-token issuance. Refreshes the sliding clock.
    public var lastUsedAt: CapsuleTimestamp
    /// When the sliding inactivity window expires — 180 days of disuse by
    /// default. Refreshes on every use.
    public var inactivityExpiresAt: CapsuleTimestamp
    /// The hard ceiling — 365 days from issuance by default. **Does not reset
    /// on use**: it caps an exfiltrated token's window regardless of activity.
    public var hardExpiresAt: CapsuleTimestamp
    public var isCurrent: Bool
    public var revokedAt: CapsuleTimestamp?

    public init(
        id: SessionID,
        deviceID: DeviceID? = nil,
        cohortHash: String? = nil,
        createdAt: CapsuleTimestamp,
        lastUsedAt: CapsuleTimestamp,
        inactivityExpiresAt: CapsuleTimestamp,
        hardExpiresAt: CapsuleTimestamp,
        isCurrent: Bool = false,
        revokedAt: CapsuleTimestamp? = nil
    ) {
        self.id = id
        self.deviceID = deviceID
        self.cohortHash = cohortHash
        self.createdAt = createdAt
        self.lastUsedAt = lastUsedAt
        self.inactivityExpiresAt = inactivityExpiresAt
        self.hardExpiresAt = hardExpiresAt
        self.isCurrent = isCurrent
        self.revokedAt = revokedAt
    }

    /// Whether the session would still be honoured at the given instant.
    public func isLive(at now: CapsuleTimestamp) -> Bool {
        revokedAt == nil && now < inactivityExpiresAt && now < hardExpiresAt
    }

    /// Whichever expiry bites first — the date a session ledger should show.
    public var effectiveExpiry: CapsuleTimestamp {
        min(inactivityExpiresAt, hardExpiresAt)
    }
}

// MARK: - DeviceCohort

/// A group of sessions and device records believed to come from one physical
/// device (*Authentication — Device Cohorts*).
///
/// A grouping aid, nothing more. The client **asserts, it does not litigate** —
/// there is deliberately no "this isn't my device" toggle, because a user
/// cannot adjudicate a hash and the value is advisory anyway. The dispute path
/// is a support report.
public struct DeviceCohort: Sendable, Equatable, Identifiable, Hashable {
    public var cohortHash: String
    public var firstSeen: CapsuleTimestamp
    public var lastSeen: CapsuleTimestamp
    /// The device ids that have reported this cohort.
    public var deviceIDs: [DeviceID]

    public var id: String { cohortHash }

    public init(
        cohortHash: String,
        firstSeen: CapsuleTimestamp,
        lastSeen: CapsuleTimestamp,
        deviceIDs: [DeviceID]
    ) {
        self.cohortHash = cohortHash
        self.firstSeen = firstSeen
        self.lastSeen = lastSeen
        self.deviceIDs = deviceIDs
    }

    /// Whether this cohort has been seen before the current enrollment — the
    /// "a device you've used before" assertion.
    public var isPreviouslySeen: Bool {
        deviceIDs.count > 1
    }
}

// MARK: - RecoveryVerificationState

/// Where the recovery-secret verification cadence stands
/// (*Backup & Recovery — Schedule and Triggers*).
///
/// The check exists because a recovery secret written on a napkin thirteen
/// months ago is a secret the user only *believes* they have. It is
/// **local-only** — no server round-trip, no guessing oracle — and it **never
/// blocks** sync, unlock, or any critical flow. A UI that gates anything on it
/// has misread the contract.
public struct RecoveryVerificationState: Sendable, Equatable, Hashable {
    /// The first interval after setup, in days.
    public static let initialIntervalDays = 7
    /// The second interval, in days.
    public static let steadyIntervalDays = 90
    /// The interval ceiling after two consecutive successes, in days.
    public static let maximumIntervalDays = 180
    /// Consecutive snoozes allowed before a persistent, non-blocking badge.
    public static let maximumConsecutiveSnoozes = 3
    /// Failures across at least two app sessions that trigger the guided
    /// re-wrap flow.
    public static let failuresBeforeGuidedRewrap = 3

    /// When the next prompt is due, or `nil` when verification is not armed —
    /// a sponsored account, or a setup that has not completed.
    public var nextDueAt: CapsuleTimestamp?
    /// The current backoff step, in days.
    public var currentIntervalDays: Int
    /// Consecutive snoozes taken. Capped at
    /// ``maximumConsecutiveSnoozes``, after which the badge persists.
    public var snoozeCount: Int
    /// Consecutive failures. At ``failuresBeforeGuidedRewrap`` the client
    /// offers guided rotation — which re-wraps the **same** master key, an O(1)
    /// escrow replacement with no data re-encryption.
    public var consecutiveFailures: Int
    /// Consecutive successes. Two of them raise the interval to the cap.
    public var consecutiveSuccesses: Int

    public init(
        nextDueAt: CapsuleTimestamp? = nil,
        currentIntervalDays: Int = RecoveryVerificationState.initialIntervalDays,
        snoozeCount: Int = 0,
        consecutiveFailures: Int = 0,
        consecutiveSuccesses: Int = 0
    ) {
        self.nextDueAt = nextDueAt
        self.currentIntervalDays = currentIntervalDays
        self.snoozeCount = snoozeCount
        self.consecutiveFailures = consecutiveFailures
        self.consecutiveSuccesses = consecutiveSuccesses
    }

    /// Whether a prompt is due at the given instant.
    public func isDue(at now: CapsuleTimestamp) -> Bool {
        guard let nextDueAt else { return false }
        return now >= nextDueAt
    }

    /// Whether further snoozing is available, or the badge is now persistent.
    public var canSnooze: Bool {
        snoozeCount < Self.maximumConsecutiveSnoozes
    }

    /// Whether the guided re-wrap flow should be offered.
    public var shouldOfferGuidedRewrap: Bool {
        consecutiveFailures >= Self.failuresBeforeGuidedRewrap
    }

    /// The interval that follows a success, per the documented backoff:
    /// 7 days → 90 days → 180-day cap after two consecutive successes.
    public var nextIntervalAfterSuccess: Int {
        consecutiveSuccesses + 1 >= 2 ? Self.maximumIntervalDays : Self.steadyIntervalDays
    }
}
