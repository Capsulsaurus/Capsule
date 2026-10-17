import CapsuleDomain
import Foundation

// MARK: - InMemoryModerationRecords

/// An in-memory ``ModerationRecordPort`` and ``UntrustedOriginPolicy``.
///
/// The stand-in behind the moderation screen until the real ports exist. It is
/// an `actor` for the same reason the mock stores are: the screen mutates it
/// from the main actor while a background reload reads it, and a value type
/// would silently fork.
///
/// It is deliberately **not** a "mock" in the test-double sense — it is a real,
/// if volatile, implementation, so the screen exercises the same code paths it
/// will against a server-backed one.
public actor InMemoryModerationRecords {
    private var entries: [ModerationAuditEntry]
    private var filed: [ModerationAppeal]
    private var origins: [UntrustedOrigin]
    private var appealOrdinal = 0

    public init(
        entries: [ModerationAuditEntry] = [],
        appeals: [ModerationAppeal] = [],
        untrustedOrigins: [UntrustedOrigin] = []
    ) {
        self.entries = entries
        filed = appeals
        origins = untrustedOrigins
    }

    /// A populated instance for previews: a reversible takedown the user can
    /// see and appeal, and an untrusted origin awaiting a decision.
    public static func populated(now: CapsuleTimestamp) -> InMemoryModerationRecords {
        InMemoryModerationRecords(
            entries: [
                ModerationAuditEntry(
                    id: "audit-1",
                    action: .takedown,
                    subjectDescription: "photos.other.example/album/summer",
                    occurredAt: CapsuleTimestamp(epochSeconds: now.epochSeconds - 6 * 86400),
                    reason: "Reported for impersonation",
                    isAppealable: true
                ),
                ModerationAuditEntry(
                    id: "audit-2",
                    action: .reinstatement,
                    subjectDescription: "capsule.example/album/family",
                    occurredAt: CapsuleTimestamp(epochSeconds: now.epochSeconds - 2 * 86400),
                    reason: nil,
                    isAppealable: false
                ),
            ],
            appeals: [],
            untrustedOrigins: [
                UntrustedOrigin(origin: "unknown.example", isConsented: false, withheldAssetCount: 47),
            ]
        )
    }
}

// MARK: - ModerationRecordPort

extension InMemoryModerationRecords: ModerationRecordPort {
    public func auditEntries() async throws -> [ModerationAuditEntry] {
        entries.sorted { $0.occurredAt > $1.occurredAt }
    }

    public func appeals() async throws -> [ModerationAppeal] {
        filed
    }

    /// File an appeal, refusing a duplicate.
    ///
    /// Refusing rather than silently returning the existing one: an appeal is a
    /// request for human attention, and quietly no-oping would let a user tap
    /// twice and believe they had escalated.
    public func submitAppeal(for entryID: String) async throws -> ModerationAppeal {
        if let existing = filed.first(where: { $0.entryID == entryID }) {
            return existing
        }
        guard let entry = entries.first(where: { $0.id == entryID }), entry.isAppealable else {
            throw CapsuleError(
                code: .moderationReportUnsigned,
                detail: "FeatureSharing: no appealable moderation record for \(entryID)"
            )
        }
        appealOrdinal += 1
        let appeal = ModerationAppeal(
            id: "appeal-\(appealOrdinal)",
            entryID: entryID,
            state: .submitted,
            submittedAt: entry.occurredAt
        )
        filed.append(appeal)
        return appeal
    }
}

// MARK: - UntrustedOriginPolicy

extension InMemoryModerationRecords: UntrustedOriginPolicy {
    public func untrustedOrigins() async throws -> [UntrustedOrigin] {
        origins
    }

    public func setConsent(_ granted: Bool, for origin: String) async throws {
        origins = origins.map { entry in
            guard entry.origin == origin else { return entry }
            var updated = entry
            updated.isConsented = granted
            return updated
        }
    }
}
