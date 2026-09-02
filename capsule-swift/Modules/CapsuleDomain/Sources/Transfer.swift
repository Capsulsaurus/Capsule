import Foundation

// MARK: - UploadPolicy

/// The per-device upload policy (*Download and Synchronization — Upload Tiering*).
///
/// The choice is **ordering, not a distinct code path**: the same upload
/// sessions, the same bundle mechanics, the same finalization run under both.
/// Under `staged` the client simply has not opened the higher-tier session yet,
/// and the server has zero mode branches. Anything in the UI that implies
/// "staged uploads are a different kind of upload" is a lie about the protocol.
public enum UploadPolicy: ClosedWireEnum {
    /// Every session of an asset's bundle opens eagerly, in any order. The
    /// default.
    case full
    /// Sessions open in tier order per asset, each tier gated by the connection
    /// class. Mutually exclusive with streaming import.
    case staged
    case unknown(String)

    public static let knownCases: [UploadPolicy] = [.full, .staged]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .full: "full"
        case .staged: "staged"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - UploadTier

/// The upload tier ladder, mirroring the download ladder
/// (*Download and Synchronization — Upload Tiering*).
///
/// Tiers map directly onto existing blob roles — **no new blob kind exists for
/// staging**, and no new wire surface. Like ``RepresentationTier`` this carries
/// no `unknown` case: it is client-side scheduling order, never a wire value,
/// and its `Comparable` order *is* the ladder.
public enum UploadTier: Int, Sendable, Equatable, Hashable, Comparable, CaseIterable, Codable {
    /// T0 — the index: signed manifest plus metadata blob with its embedded
    /// LQIP. A few KB per asset; escapes on **any** usable connection, even
    /// constrained or adverse. This is the rung that means "if the phone
    /// drowns, the user knows exactly what was lost and holds a preview of it".
    case index = 0
    /// T1 — thumbnail and preview derivatives. Needs a non-metered link.
    case preview = 1
    /// T2 — the original. Needs unmetered Wi-Fi or an explicit force-sync. Its
    /// finalization flips `original_held` and unlocks every release path.
    case original = 2

    public static func < (lhs: UploadTier, rhs: UploadTier) -> Bool {
        lhs.rawValue < rhs.rawValue
    }

    /// The tiers in strict ladder order — the ordering input a staged scheduler
    /// iterates.
    public static let ladder: [UploadTier] = [.index, .preview, .original]

    /// Whether this tier's session may open on the given connection.
    ///
    /// Expressed as a predicate rather than a "minimum class" because the
    /// classes are **not** a total order — `constrained` and `adverse` are
    /// different kinds of bad, not different amounts of it. T0 is deliberately
    /// permissive: a few KB of index must escape the device on any usable link,
    /// because that index is what turns a lost phone into a known loss rather
    /// than an unknown one.
    public func canOpen(on connection: ConnectionClass, forceSync: Bool = false) -> Bool {
        guard connection.isUsable else { return false }
        if forceSync { return true }
        switch self {
        case .index: return true
        case .preview: return connection.permitsSmallReconciliation
        case .original: return connection.permitsBulkTransfer
        }
    }
}

// MARK: - UploadSessionState

/// The upload session state machine (*Upload Protocol — Session State Machine*).
///
/// ```text
/// pending ─▶ uploading ─▶ waitingForProcessing ─▶ completed
///                                             └─▶ failedProcessing
/// ```
///
/// Both terminal states are **receipts**, not disappearances: a client whose
/// finalization acknowledgement was lost re-queries and learns the upload
/// already succeeded or failed, instead of seeing a vanished session and
/// blindly re-uploading.
public enum UploadSessionState: ClosedWireEnum {
    /// Session created, no bytes received.
    case pending
    /// At least one chunk accepted; transfer in progress.
    case uploading
    /// Every declared byte received; hash verification is running. **Not
    /// evictable, not cancellable** — finalization is never interrupted.
    case waitingForProcessing
    /// Hash verified, asset marked uploaded. Terminal.
    case completed
    /// Terminal failure: hash mismatch, size mismatch, or envelope
    /// re-validation failure. The upload file and pending row are gone; the
    /// status record survives as a receipt.
    case failedProcessing
    case unknown(String)

    public static let knownCases: [UploadSessionState] = [
        .pending, .uploading, .waitingForProcessing, .completed, .failedProcessing,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .pending: "pending"
        case .uploading: "uploading"
        case .waitingForProcessing: "waiting_for_processing"
        case .completed: "completed"
        case .failedProcessing: "failed_processing"
        case let .unknown(raw): raw
        }
    }

    /// Whether the session has reached a terminal state.
    public var isTerminal: Bool {
        self == .completed || self == .failedProcessing
    }

    /// Whether the session may still be cancelled. Cancellation is refused once
    /// finalization has begun.
    public var isCancellable: Bool {
        self == .pending || self == .uploading
    }
}

// MARK: - UploadSession

/// One in-flight or terminal upload session, as a resuming client sees it.
public struct UploadSession: Sendable, Equatable, Identifiable, Hashable {
    public var id: UploadID
    /// The asset this blob belongs to.
    public var assetID: String
    /// What kind of blob this session carries.
    public var blobRole: BlobRole
    /// Which rung of the staged ladder this session is.
    public var tier: UploadTier
    public var state: UploadSessionState
    /// The authoritative next expected byte, from the server.
    public var offset: UInt64
    /// The declared total, immutable after session creation.
    public var declaredSize: UInt64
    /// The content address the server will verify against.
    public var ciphertextHash: BlobHash

    public init(
        id: UploadID,
        assetID: String,
        blobRole: BlobRole,
        tier: UploadTier,
        state: UploadSessionState,
        offset: UInt64,
        declaredSize: UInt64,
        ciphertextHash: BlobHash
    ) {
        self.id = id
        self.assetID = assetID
        self.blobRole = blobRole
        self.tier = tier
        self.state = state
        self.offset = offset
        self.declaredSize = declaredSize
        self.ciphertextHash = ciphertextHash
    }

    /// Transferred fraction, 0…1. Zero for a zero-size declaration, which the
    /// protocol forbids anyway.
    public var fractionComplete: Double {
        declaredSize == 0 ? 0 : min(1, Double(offset) / Double(declaredSize))
    }
}

// MARK: - ConnectionClass

/// The connection-class taxonomy (*Networking — Connection Classes*).
///
/// The input to sync criteria, staged-upload tier gates, cache budgets,
/// prefetch, and adaptive chunk sizing. ``adverse`` is deliberately
/// **behavioural**, not reported by any OS: it is promoted after enough
/// mid-transfer resets, stalls, or black-holes in a sliding window, because the
/// networks that need it most look perfectly "connected" to every OS API.
public enum ConnectionClass: ClosedWireEnum {
    /// Bulk transfer is acceptable.
    case unmetered
    /// A byte-counted link; bulk work is deferred.
    case metered
    /// OS-level data saver is active.
    case constrained
    /// Connectivity is present but unreliable.
    case adverse
    /// No usable path.
    case offline
    case unknown(String)

    public static let knownCases: [ConnectionClass] = [
        .unmetered, .metered, .constrained, .adverse, .offline,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .unmetered: "unmetered"
        case .metered: "metered"
        case .constrained: "constrained"
        case .adverse: "adverse"
        case .offline: "offline"
        case let .unknown(raw): raw
        }
    }

    /// Whether any request can be made at all.
    public var isUsable: Bool {
        self != .offline && !isUnknownClass
    }

    /// Whether a large reconciliation — bulk uploads, original-tier downloads —
    /// may proceed without an explicit force-sync.
    public var permitsBulkTransfer: Bool {
        self == .unmetered
    }

    /// Whether a small reconciliation may proceed proactively. Any non-metered
    /// usable class qualifies.
    public var permitsSmallReconciliation: Bool {
        self == .unmetered || self == .adverse
    }

    private var isUnknownClass: Bool {
        if case .unknown = self { return true }
        return false
    }
}

// MARK: - RetryClass

/// The retry policy classes (*Networking — Retry Policy Classes*).
///
/// Named so per-surface retry ladders are instances of a shared shape rather
/// than reinventions. The critical property of ``controlCeremony`` is that it
/// **never abandons silently** — a key ceremony that gives up quietly leaves the
/// user locked out of their own data with no signal.
public enum RetryClass: ClosedWireEnum {
    /// Short timeout, at most two retries, then a visible failure state. Auth
    /// flows, on-demand tier fetches.
    case interactive
    /// Resume-first via offset or range, patient within the session's lifetime,
    /// backing off between attempts. Uploads and downloads.
    case bulkTransfer
    /// A slow ladder over a long horizon that never abandons silently. MLS
    /// recovery, the federation circuit breaker.
    case controlCeremony
    case unknown(String)

    public static let knownCases: [RetryClass] = [.interactive, .bulkTransfer, .controlCeremony]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .interactive: "interactive"
        case .bulkTransfer: "bulk_transfer"
        case .controlCeremony: "control_ceremony"
        case let .unknown(raw): raw
        }
    }

    /// Whether the class gives up and surfaces a failure, as opposed to
    /// retrying indefinitely on a long horizon.
    public var abandonsOnExhaustion: Bool {
        self != .controlCeremony
    }
}
