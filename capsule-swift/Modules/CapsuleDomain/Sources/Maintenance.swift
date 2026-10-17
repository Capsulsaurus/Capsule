import Foundation

// MARK: - MaintenanceTaskKind

/// The scheduled integrity and housekeeping jobs (*Filesystem — Maintenance*).
///
/// Split by cost, because the gating differs: a cheap structural check can run
/// on any wake-up, while a deep content validation is idle-and-on-power work. A
/// UI that offers "verify my library" must say which one it is starting.
public enum MaintenanceTaskKind: ClosedWireEnum {
    /// Reconcile the local index against what is actually on disk.
    case indexReconciliation
    /// Cheap structural checks — a `stat` and an index lookup per blob.
    case structuralValidation
    /// Re-read and re-hash local bytes to catch silent bit-rot. Expensive;
    /// idle-and-on-power only.
    case deepContentValidation
    /// Collapse byte-identical assets that slipped in through overlapping
    /// imports or a restore over an existing library.
    case intraLibraryDeduplication
    /// Evict re-fetchable cached tiers under the byte budget. **Never** touches
    /// a device-owned original that has not been confirmed durable.
    case cacheEviction
    /// Purge trash whose signed retention window has elapsed.
    case trashPurge
    case unknown(String)

    public static let knownCases: [MaintenanceTaskKind] = [
        .indexReconciliation, .structuralValidation, .deepContentValidation,
        .intraLibraryDeduplication, .cacheEviction, .trashPurge,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .indexReconciliation: "index_reconciliation"
        case .structuralValidation: "structural_validation"
        case .deepContentValidation: "deep_content_validation"
        case .intraLibraryDeduplication: "intra_library_deduplication"
        case .cacheEviction: "cache_eviction"
        case .trashPurge: "trash_purge"
        case let .unknown(raw): raw
        }
    }

    /// Whether the task is expensive enough to require idle-and-on-power
    /// gating.
    public var requiresIdleAndPower: Bool {
        self == .deepContentValidation || self == .intraLibraryDeduplication
    }

    /// Whether the task can destroy local bytes, and so must run behind the
    /// verify-before-destroy gate.
    public var isDestructive: Bool {
        self == .cacheEviction || self == .trashPurge
    }
}

// MARK: - MaintenanceTask

/// One maintenance job's current standing.
public struct MaintenanceTask: Sendable, Equatable, Identifiable, Hashable {
    /// How the job is doing.
    public enum State: Sendable, Equatable, Hashable {
        case idle
        case running(fractionComplete: Double)
        /// Finished; `findingCount` is what it turned up, zero being the good
        /// answer.
        case completed(occurredAt: CapsuleTimestamp, findingCount: Int)
        /// Failed. The reason is a stable code, never prose.
        case failed(occurredAt: CapsuleTimestamp, code: ErrorCode)
        /// Waiting for its gating conditions — idle, on power, unmetered.
        case waitingForConditions
    }

    public var kind: MaintenanceTaskKind
    public var state: State
    public var lastRunAt: CapsuleTimestamp?

    public var id: MaintenanceTaskKind { kind }

    public init(kind: MaintenanceTaskKind, state: State, lastRunAt: CapsuleTimestamp? = nil) {
        self.kind = kind
        self.state = state
        self.lastRunAt = lastRunAt
    }
}

// MARK: - LibrarySettings

/// The per-device and per-library preferences the settings surface edits.
///
/// Deliberately **no user-facing strings** here: every field is a value or an
/// identifier, and the labels come from the i18n catalog. What lives here is
/// only what changes behaviour.
///
/// The per-owner *collaborative* settings — smart-album definitions, scope
/// overrides, aggregated-album covers — are a different thing entirely: they
/// live in the E2E-encrypted library-settings document, sync across devices as
/// CRDTs, and are reached through their own ports. These are the local knobs.
public struct LibrarySettings: Sendable, Equatable, Hashable {
    /// What is fetched eagerly.
    public var syncScope: SyncScope
    /// How this device orders its upload sessions.
    public var uploadPolicy: UploadPolicy
    /// Whether background sync is enabled at all. Platforms that cannot detect
    /// metered connections do not offer it, rather than guessing.
    public var autoSyncEnabled: Bool
    /// The local cache byte budget, when the user has set one.
    public var cacheBudgetBytes: UInt64?
    /// Whether on-device ML runs.
    public var aiProcessingEnabled: Bool
    /// Whether ML work waits for power.
    public var aiRequiresPower: Bool
    /// Whether the two-week staleness notification is enabled. Disabling opts
    /// out of the **warning** only; it does not affect auto sync itself.
    public var stalenessNotificationEnabled: Bool
    /// The default trash retention for new deletes, in days.
    public var defaultRetentionDays: Int
    /// Whether diagnostics may be collected. Off unless explicitly granted.
    public var diagnosticsEnabled: Bool

    public init(
        syncScope: SyncScope = .metadataAndThumbnails,
        uploadPolicy: UploadPolicy = .full,
        autoSyncEnabled: Bool = true,
        cacheBudgetBytes: UInt64? = nil,
        aiProcessingEnabled: Bool = true,
        aiRequiresPower: Bool = true,
        stalenessNotificationEnabled: Bool = true,
        defaultRetentionDays: Int = TrashEntry.defaultRetentionDays,
        diagnosticsEnabled: Bool = false
    ) {
        self.syncScope = syncScope
        self.uploadPolicy = uploadPolicy
        self.autoSyncEnabled = autoSyncEnabled
        self.cacheBudgetBytes = cacheBudgetBytes
        self.aiProcessingEnabled = aiProcessingEnabled
        self.aiRequiresPower = aiRequiresPower
        self.stalenessNotificationEnabled = stalenessNotificationEnabled
        self.defaultRetentionDays = defaultRetentionDays
        self.diagnosticsEnabled = diagnosticsEnabled
    }
}

// MARK: - SyncStatus

/// Where the whole library stands with its server (*Download and
/// Synchronization — Auto Syncing*).
///
/// The two-week staleness rule is a **product surface, not a bug**: a mobile OS
/// may grant no background window for days, and a library silently falling out
/// of date defeats the point of keeping content safe elsewhere. So it is
/// surfaced, with a one-tap force sync that proceeds regardless of the metered
/// criteria on the user's explicit consent.
public struct SyncStatus: Sendable, Equatable, Hashable {
    /// How long without a completed sync before the user is told, in days.
    public static let stalenessThresholdDays = 14

    /// When the last reconciliation completed.
    public var lastCompletedSyncAt: CapsuleTimestamp?
    /// Local changes not yet on the server.
    public var pendingUploadCount: Int
    /// Server changes not yet applied locally.
    public var pendingDownloadCount: Int
    /// The current connection class.
    public var connectionClass: ConnectionClass
    /// Whether a reconciliation is running now.
    public var isSyncing: Bool
    /// When the staleness notification is snoozed until, if it is.
    public var staleNotificationSnoozedUntil: CapsuleTimestamp?

    public init(
        lastCompletedSyncAt: CapsuleTimestamp? = nil,
        pendingUploadCount: Int = 0,
        pendingDownloadCount: Int = 0,
        connectionClass: ConnectionClass = .unmetered,
        isSyncing: Bool = false,
        staleNotificationSnoozedUntil: CapsuleTimestamp? = nil
    ) {
        self.lastCompletedSyncAt = lastCompletedSyncAt
        self.pendingUploadCount = pendingUploadCount
        self.pendingDownloadCount = pendingDownloadCount
        self.connectionClass = connectionClass
        self.isSyncing = isSyncing
        self.staleNotificationSnoozedUntil = staleNotificationSnoozedUntil
    }

    /// Whether anything is outstanding in either direction.
    public var hasPendingWork: Bool {
        pendingUploadCount > 0 || pendingDownloadCount > 0
    }

    /// Whether the library is behind by the documented threshold **and** has
    /// un-synced changes.
    ///
    /// Both halves are required: a library with nothing to sync is not stale,
    /// however long it has been, and warning about it would be noise.
    public func isStale(at now: CapsuleTimestamp) -> Bool {
        guard hasPendingWork else { return false }
        if let snoozed = staleNotificationSnoozedUntil, now < snoozed { return false }
        guard let last = lastCompletedSyncAt else { return true }
        let threshold = Int64(Self.stalenessThresholdDays) * 86400
        return (now.epochSeconds - last.epochSeconds) > threshold
    }

    /// Whether a large reconciliation would proceed right now without an
    /// explicit force sync.
    public var canRunLargeReconciliation: Bool {
        connectionClass.permitsBulkTransfer
    }
}

// MARK: - ModerationReport

/// A report a user files about content or a peer (*Moderation*).
///
/// Carries no report body text in this layer — the composed message is user
/// input handled at the edge, and everything here is structured so a report can
/// be rate-limited, routed, and audited without reading prose.
public struct ModerationReport: Sendable, Equatable, Identifiable, Hashable {
    /// What is being reported.
    public enum Subject: Sendable, Equatable, Hashable {
        case asset(String)
        case album(String)
        case user(handle: String)
        case peer(PeerID)
    }

    /// The closed reason set.
    public enum Reason: ClosedWireEnum {
        case abuse
        case spam
        case impersonation
        case illegalContent
        case other
        case unknown(String)

        public static let knownCases: [Reason] = [
            .abuse, .spam, .impersonation, .illegalContent, .other,
        ]

        public init(rawValue: String) {
            self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
        }

        public var rawValue: String {
            switch self {
            case .abuse: "abuse"
            case .spam: "spam"
            case .impersonation: "impersonation"
            case .illegalContent: "illegal_content"
            case .other: "other"
            case let .unknown(raw): raw
            }
        }
    }

    public var id: String
    public var subject: Subject
    public var reason: Reason
    public var submittedAt: CapsuleTimestamp

    public init(id: String, subject: Subject, reason: Reason, submittedAt: CapsuleTimestamp) {
        self.id = id
        self.subject = subject
        self.reason = reason
        self.submittedAt = submittedAt
    }
}

// MARK: - BlockEntry

/// A user's own blocklist entry. Blocking drops the subject's constituent from
/// this viewer's aggregated albums, per-origin.
public struct BlockEntry: Sendable, Equatable, Identifiable, Hashable {
    public enum Subject: Sendable, Equatable, Hashable {
        case user(handle: String)
        case peer(PeerID)
    }

    public var id: String
    public var subject: Subject
    public var blockedAt: CapsuleTimestamp

    public init(id: String, subject: Subject, blockedAt: CapsuleTimestamp) {
        self.id = id
        self.subject = subject
        self.blockedAt = blockedAt
    }
}
