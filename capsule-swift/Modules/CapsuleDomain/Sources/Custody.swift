import Foundation

// MARK: - CustodyReceipt

/// The server's signed acknowledgement that it accepted custody of exactly
/// these bytes (*Storage Verification — Custody Receipts*).
///
/// This is what makes "the server lost my photo" and "the client never uploaded
/// it" distinguishable rather than symmetric unfalsifiable claims. The receipt
/// is the server-signed complement of the client-signed provenance chain: the
/// envelope proves what a client *claimed and signed*, the receipt proves what
/// the server *accepted*, over a hash **it recomputed itself** and never echoed
/// from the client.
///
/// Receipts are permanent, so a client can fetch one long after the session
/// record expires. A client must hold and have verified the receipt before
/// releasing any irreplaceable local bytes — a server that withholds receipts
/// never becomes the sole holder of an only copy.
public struct CustodyReceipt: Sendable, Equatable, Hashable, Identifiable {
    public var version: String
    public var cryptoSuiteID: UInt16
    public var protocolVersion: String
    /// The server's canonical origin — binds the receipt to one server.
    public var serverID: String
    /// Fingerprint of the attestation key that signed; survives rotation.
    public var serverKeyID: Data
    /// Strictly monotonic per server. A client holding `receipt_seq = N` proves
    /// the log has at least `N` entries, which bounds silent truncation.
    public var receiptSequence: UInt64
    /// Hash of the previous receipt in the server's log; `nil` only for the
    /// first — the provenance chain's append-only discipline applied to the
    /// server's own log.
    public var priorReceiptHash: String?
    /// The session that produced custody.
    public var uploadID: UploadID
    public var assetID: String
    public var blobRole: BlobRole
    /// The digest the **server** recomputed at finalization.
    public var ciphertextHash: BlobHash
    public var size: UInt64
    /// Hash of the manifest envelope, binding the receipt to the asset's
    /// provenance-chain position.
    public var envelopeHash: String?
    public var uploadedByUser: String
    public var uploadedByDevice: DeviceID?
    /// The server's trusted clock at the finalization commit.
    public var receivedAt: CapsuleTimestamp
    /// The server's hybrid signature over every field above. Opaque here;
    /// verification belongs to `capsule-core`.
    public var serverSignature: Data

    public var id: String { "\(serverID)#\(receiptSequence)" }

    public init(
        version: String,
        cryptoSuiteID: UInt16,
        protocolVersion: String,
        serverID: String,
        serverKeyID: Data,
        receiptSequence: UInt64,
        priorReceiptHash: String? = nil,
        uploadID: UploadID,
        assetID: String,
        blobRole: BlobRole,
        ciphertextHash: BlobHash,
        size: UInt64,
        envelopeHash: String? = nil,
        uploadedByUser: String,
        uploadedByDevice: DeviceID? = nil,
        receivedAt: CapsuleTimestamp,
        serverSignature: Data
    ) {
        self.version = version
        self.cryptoSuiteID = cryptoSuiteID
        self.protocolVersion = protocolVersion
        self.serverID = serverID
        self.serverKeyID = serverKeyID
        self.receiptSequence = receiptSequence
        self.priorReceiptHash = priorReceiptHash
        self.uploadID = uploadID
        self.assetID = assetID
        self.blobRole = blobRole
        self.ciphertextHash = ciphertextHash
        self.size = size
        self.envelopeHash = envelopeHash
        self.uploadedByUser = uploadedByUser
        self.uploadedByDevice = uploadedByDevice
        self.receivedAt = receivedAt
        self.serverSignature = serverSignature
    }
}

// MARK: - StorageVerification

/// Per-blob storage facts, all three of which the server can attest **without
/// any key** (*Storage Verification — What "Safely Stored" Means*).
public struct BlobVerdict: Sendable, Equatable, Hashable, Identifiable {
    public var hash: BlobHash
    public var role: BlobRole
    /// Present in the blob store at its content address — not merely an
    /// in-flight chunk.
    public var stored: Bool
    /// Referenced by a committed, `uploaded = true` row with a current chain
    /// head.
    public var indexed: Bool
    /// In a state the server would actually serve: referenced, not mid-GC, not
    /// quarantined.
    public var retrievable: Bool

    public var id: String { hash.rawValue }

    public init(hash: BlobHash, role: BlobRole, stored: Bool, indexed: Bool, retrievable: Bool) {
        self.hash = hash
        self.role = role
        self.stored = stored
        self.indexed = indexed
        self.retrievable = retrievable
    }

    /// All three facts hold.
    public var isFullyHeld: Bool {
        stored && indexed && retrievable
    }
}

/// One asset's storage verdict — the query that gates every destructive local
/// action (*Storage Verification*).
///
/// A hash the client lists that the server does not associate with the asset
/// comes back `stored = false, indexed = false` — **surfaced, never silently
/// omitted**, so a missing blob cannot be mistaken for one that was never asked
/// about.
///
/// The verdict is a **point-in-time fact**, not a standing guarantee, which is
/// why the verify → release window is kept tight on both sides.
public struct StorageVerification: Sendable, Equatable, Hashable, Identifiable {
    /// How long a verdict may be relied on before it must be re-taken.
    public static let verdictFreshnessSeconds: Int64 = 60

    public var assetID: String
    /// Every **required** blob is stored, indexed, and retrievable.
    public var durable: Bool
    public var blobs: [BlobVerdict]
    /// The server's trusted clock when the check ran.
    public var checkedAt: CapsuleTimestamp

    public var id: String { assetID }

    public init(
        assetID: String,
        durable: Bool,
        blobs: [BlobVerdict],
        checkedAt: CapsuleTimestamp
    ) {
        self.assetID = assetID
        self.durable = durable
        self.blobs = blobs
        self.checkedAt = checkedAt
    }

    /// Whether this verdict is fresh enough to authorise a release at `now`.
    ///
    /// A stale `durable` must never authorise dropping the only copy of
    /// something; the client re-verifies rather than trusting an old yes.
    public func authorisesRelease(at now: CapsuleTimestamp) -> Bool {
        durable && (now.epochSeconds - checkedAt.epochSeconds) <= Self.verdictFreshnessSeconds
    }

    /// The blobs that are not fully held — what a "not yet confirmed on server"
    /// surface itemises.
    public var missingBlobs: [BlobVerdict] {
        blobs.filter { !$0.isFullyHeld }
    }
}
