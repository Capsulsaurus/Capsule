import CapsuleDomain
import Foundation

// MARK: - ModerationAction

/// What a moderator did (*Moderation*).
///
/// Every one of these must produce a record the affected user can see: "No
/// silent operations" is the doc's global rule, and a takedown that merely made
/// an asset stop loading would leave the user guessing.
public enum ModerationAction: String, Sendable, Equatable, Hashable, CaseIterable {
    /// Serving stopped. The **bytes are not deleted** — the user can still
    /// restore from their own backup — and it is reversible by default.
    case takedown
    /// A takedown held open until a legal obligation ends.
    case legalHold
    /// The account cannot create sessions, albums, or links. Data is untouched.
    case accountSuspension
    /// A previous action was lifted.
    case reinstatement
}

// MARK: - ModerationAuditEntry

/// One row of the user's moderation audit log.
///
/// The reason is optional because policy does not always permit disclosing it —
/// "where policy permits, why". An absent reason is rendered as absent, never as
/// a guess, and never as an empty row that looks like a rendering bug.
public struct ModerationAuditEntry: Sendable, Equatable, Identifiable, Hashable {
    public var id: String
    public var action: ModerationAction
    /// What it applied to, as the user would recognise it.
    public var subjectDescription: String
    public var occurredAt: CapsuleTimestamp
    /// The disclosed reason, when there is one.
    public var reason: String?
    /// Whether this action can still be appealed.
    public var isAppealable: Bool

    public init(
        id: String,
        action: ModerationAction,
        subjectDescription: String,
        occurredAt: CapsuleTimestamp,
        reason: String? = nil,
        isAppealable: Bool = true
    ) {
        self.id = id
        self.action = action
        self.subjectDescription = subjectDescription
        self.occurredAt = occurredAt
        self.reason = reason
        self.isAppealable = isAppealable
    }
}

// MARK: - ModerationAppeal

/// An appeal against a moderation action.
///
/// Authenticated by **master-key proof, not a session token** — the session may
/// be the very thing under dispute — which is why the flow has to stay reachable
/// from a suspended account.
public struct ModerationAppeal: Sendable, Equatable, Identifiable, Hashable {
    public enum State: String, Sendable, Equatable, Hashable, CaseIterable {
        /// Filed and sitting in the home server's admin queue.
        case submitted
        /// An admin is looking at it.
        case underReview
        /// Granted: the constraint is simply lifted.
        case granted
        /// Declined. The original record stands.
        case declined
    }

    public var id: String
    /// The audit entry appealed against.
    public var entryID: String
    public var state: State
    public var submittedAt: CapsuleTimestamp

    public init(id: String, entryID: String, state: State, submittedAt: CapsuleTimestamp) {
        self.id = id
        self.entryID = entryID
        self.state = state
        self.submittedAt = submittedAt
    }
}

// MARK: - ModerationRecordPort

/// The audit-log and appeal seam.
///
/// A **module-local port**, not a `CapsulePorts` protocol: neither the audit log
/// nor appeals has a `CapsulePorts` surface yet, and inventing one in a feature
/// module would put a cross-cutting contract in the wrong place. Declaring it
/// here keeps the screen buildable and unit-testable now, and gives the eventual
/// port an exact shape to match.
public protocol ModerationRecordPort: Sendable {
    /// Moderation actions affecting this user, newest first.
    func auditEntries() async throws -> [ModerationAuditEntry]
    /// Appeals this user has filed.
    func appeals() async throws -> [ModerationAppeal]
    /// File an appeal. The caller must already have satisfied the master-key
    /// proof; this seam does not model the ceremony.
    func submitAppeal(for entryID: String) async throws -> ModerationAppeal
}

// MARK: - UntrustedOriginPolicy

/// Which untrusted origins this user has explicitly consented to load from.
///
/// *Federation*: "If an album contains assets from an untrusted external server,
/// clients skip loading them unless the user explicitly consents, accepting the
/// risk." Default-deny, per origin, and reversible — consent is a decision the
/// user makes with the risk stated, not a preference that quietly accumulates.
public protocol UntrustedOriginPolicy: Sendable {
    /// Origins that are not on a trust path, with whether consent was given.
    func untrustedOrigins() async throws -> [UntrustedOrigin]
    /// Grant or withdraw consent for one origin.
    func setConsent(_ granted: Bool, for origin: String) async throws
}

/// One origin the client will not load from without an explicit decision.
public struct UntrustedOrigin: Sendable, Equatable, Identifiable, Hashable {
    public var origin: String
    public var isConsented: Bool
    /// How many local index entries are being withheld pending consent. They
    /// are withheld, not discarded.
    public var withheldAssetCount: Int

    public var id: String { origin }

    public init(origin: String, isConsented: Bool, withheldAssetCount: Int) {
        self.origin = origin
        self.isConsented = isConsented
        self.withheldAssetCount = withheldAssetCount
    }
}
