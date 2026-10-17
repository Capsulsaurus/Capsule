import Foundation

// MARK: - QuotaState

/// The five quota states (*Quota — Threshold Model*).
///
/// The states are a ladder of what stops working, and the order matters because
/// the product promise is that **a user can always delete their way back under
/// quota**. Reads, deletes, and restore-from-trash keep working in every state
/// including ``graceExpired``; the provenance and metadata writes those
/// operations themselves produce are always admitted.
public enum QuotaState: ClosedWireEnum {
    /// `used < softLimit`. Everything works.
    case withinQuota
    /// `softLimit ≤ used < hardLimit`. Uploads still succeed; the UI warns.
    case softWarning
    /// `used ≥ hardLimit`. New uploads are rejected at session creation.
    /// Metadata edits and every other write still work; existing assets stay
    /// accessible.
    case hardExceeded
    /// Over the hard limit for longer than the grace window. Adds to
    /// ``hardExceeded``: metadata-growth writes — caption and tag edits, new
    /// share or upload links — are refused too. Freeing space lifts it.
    case graceExpired
    /// An admin or billing action. Server-defined; possibly upload refusal,
    /// possibly full lockout.
    case suspended
    case unknown(String)

    public static let knownCases: [QuotaState] = [
        .withinQuota, .softWarning, .hardExceeded, .graceExpired, .suspended,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .withinQuota: "ok"
        case .softWarning: "soft_warning"
        case .hardExceeded: "hard_exceeded"
        case .graceExpired: "grace_expired"
        case .suspended: "suspended"
        case let .unknown(raw): raw
        }
    }

    /// Whether a new upload session may be created.
    ///
    /// This is the **only** hard enforcement point: once a session is open the
    /// declared size is the cap and the session is allowed to complete.
    public var permitsNewUploads: Bool {
        self == .withinQuota || self == .softWarning
    }

    /// Whether a write that grows stored metadata — a caption, a tag, a new
    /// share link — is admitted.
    public var permitsMetadataGrowth: Bool {
        switch self {
        case .withinQuota, .softWarning, .hardExceeded: true
        case .graceExpired, .suspended, .unknown: false
        }
    }

    /// Whether a delete, a trash-restore, or a trash-empty is admitted.
    ///
    /// **Always true except under suspension.** A user who cannot delete cannot
    /// recover, which would turn a full account into a permanently full one.
    public var permitsReclaimingWrites: Bool {
        self != .suspended
    }

    /// Whether the user should be shown a storage warning at all.
    public var warrantsWarning: Bool {
        self != .withinQuota
    }
}

// MARK: - QuotaStatus

/// The account's storage position (*Quota — Contract Skeleton*).
///
/// Quota is accounted to the **uploader**, not the asset's owner, so a user
/// uploading on another's behalf is charged for it. Content-addressed dedup is
/// global and counts against only the *first* uploader — which is what stops a
/// malicious user racking up someone else's quota by re-uploading their assets.
public struct QuotaStatus: Sendable, Equatable, Hashable {
    /// The default grace window before ``QuotaState/graceExpired``, in days.
    public static let defaultGraceWindowDays = 14

    /// Bytes attributed to this uploader: ciphertext, metadata blobs,
    /// derivatives they generated, and provenance blobs.
    public var used: UInt64
    /// The warning threshold.
    public var softLimit: UInt64
    /// The refusal threshold. A self-hosted deployment may run with no quota,
    /// in which case this is `UInt64.max`.
    public var hardLimit: UInt64
    /// The authoritative state, as the server reports it.
    public var state: QuotaState
    /// When usage first crossed the hard limit, if it has. The clock the grace
    /// window runs against.
    public var hardExceededSince: CapsuleTimestamp?

    public init(
        used: UInt64,
        softLimit: UInt64,
        hardLimit: UInt64,
        state: QuotaState,
        hardExceededSince: CapsuleTimestamp? = nil
    ) {
        self.used = used
        self.softLimit = softLimit
        self.hardLimit = hardLimit
        self.state = state
        self.hardExceededSince = hardExceededSince
    }

    /// Bytes still available before the hard limit, floored at zero.
    public var remaining: UInt64 {
        used >= hardLimit ? 0 : hardLimit - used
    }

    /// Fraction of the hard limit consumed, 0…1. Zero for an unlimited
    /// deployment rather than a meaningless sliver.
    public var fractionUsed: Double {
        guard hardLimit > 0, hardLimit != .max else { return 0 }
        return min(1, Double(used) / Double(hardLimit))
    }

    /// Whether this deployment enforces a quota at all.
    public var isUnlimited: Bool {
        hardLimit == .max
    }

    /// Derive the state a *client* would expect from the numbers, for the
    /// mock and for asserting the server's answer is self-consistent.
    ///
    /// The server remains authoritative — ``QuotaState/suspended`` is an
    /// administrative fact no client can compute, so it is passed in rather
    /// than derived. Everything else follows the documented thresholds.
    public static func derivedState(
        used: UInt64,
        softLimit: UInt64,
        hardLimit: UInt64,
        hardExceededSince: CapsuleTimestamp?,
        now: CapsuleTimestamp,
        graceWindowDays: Int = QuotaStatus.defaultGraceWindowDays,
        isSuspended: Bool = false
    ) -> QuotaState {
        if isSuspended { return .suspended }
        guard used >= softLimit else { return .withinQuota }
        guard used >= hardLimit else { return .softWarning }
        guard let since = hardExceededSince else { return .hardExceeded }
        let graceSeconds = Int64(graceWindowDays) * 86400
        return (now.epochSeconds - since.epochSeconds) > graceSeconds ? .graceExpired : .hardExceeded
    }
}

// MARK: - LocalStorageBreakdown

/// What this device's copy of the library is spending disk on.
///
/// Split by tier because the remedies differ: evicting thumbnails saves little
/// and costs re-fetches, while releasing originals that are already durable on
/// the server saves a lot and costs nothing.
///
/// **Trash counts fully against quota until hard purge** — an asset in the trash
/// is still stored, at full size. The UI highlights the trash segment precisely
/// because "empty the trash" is the highest-leverage action a user over quota
/// can take, and the number is otherwise invisible.
public struct LocalStorageBreakdown: Sendable, Equatable, Hashable {
    /// Bytes held per representation tier.
    public var bytesByTier: [RepresentationTier: UInt64]
    /// Bytes held by soft-deleted assets still inside their retention window.
    /// Counted in ``totalBytes``, and **also** counted by the server against
    /// quota.
    public var trashBytes: UInt64
    /// Bytes this device holds as the sole durable copy — a device-owned
    /// original not yet confirmed on the server. **Exempt from automatic
    /// eviction**; only a durable verdict may release it.
    public var unreleasedOriginalBytes: UInt64
    /// Free space on the volume, when the platform reports it.
    public var availableDiskBytes: UInt64?

    public init(
        bytesByTier: [RepresentationTier: UInt64] = [:],
        trashBytes: UInt64 = 0,
        unreleasedOriginalBytes: UInt64 = 0,
        availableDiskBytes: UInt64? = nil
    ) {
        self.bytesByTier = bytesByTier
        self.trashBytes = trashBytes
        self.unreleasedOriginalBytes = unreleasedOriginalBytes
        self.availableDiskBytes = availableDiskBytes
    }

    /// Everything this device holds for the library.
    public var totalBytes: UInt64 {
        bytesByTier.values.reduce(0, +)
    }

    /// Bytes that can be reclaimed right now with no data loss: cached tiers
    /// that are re-fetchable, excluding the unreleased originals.
    public var reclaimableBytes: UInt64 {
        let cached = totalBytes
        return cached > unreleasedOriginalBytes ? cached - unreleasedOriginalBytes : 0
    }
}
