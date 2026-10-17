import CapsuleFoundation
import Foundation

// MARK: - ShareScope

/// What a share link points at (*Share Links*).
///
/// The URL's opaque id carries **no** scope — the server resolves scope from the
/// link record — so the link itself leaks nothing about what it grants or how
/// big it is.
public enum ShareScope: Sendable, Equatable, Hashable {
    /// A single asset. The recipient receives that asset's per-file key, so no
    /// album key is involved.
    case asset(AssetID)
    /// A whole album. The recipient receives the AMK for every epoch the
    /// album's history policy covers.
    case album(AlbumID)

    /// Whether this scope hands over album-wide material rather than one file
    /// key — the thing a confirmation sheet must make unmistakable.
    public var isAlbumWide: Bool {
        if case .album = self { return true }
        return false
    }
}

// MARK: - ShareLink

/// An issued share link (*Share Links*).
///
/// Two secrets, with very different lifetimes and audiences:
///
/// - ``opaqueID`` is the URL path component: 128 random bits, deliberately
///   **not** a UUIDv7, because a structured id in a URL leaks creation ordering.
/// - ``secret`` is the URL *fragment*. It **never leaves the client** — a
///   fragment is not sent to the server by any browser — so the server holds
///   only material it cannot open.
///
/// Both are secrets. Anything that logs, screenshots, or analytics-reports a
/// ``ShareLink`` is exfiltrating access.
public struct ShareLink: Sendable, Equatable, Identifiable, Hashable {
    /// The owner-held revocation handle.
    public var id: ShareID
    /// The URL path component, lowercase hex. Unstructured by design.
    public var opaqueID: String
    /// The URL fragment secret, lowercase hex. Never sent to the server.
    public var secret: String
    /// What the link grants.
    public var scope: ShareScope
    /// RFC 3339 expiry, if any. Revocation applies regardless.
    public var expiresAt: CapsuleTimestamp?
    /// Whether an Argon2id passphrase layer additionally wraps the material.
    /// The passphrase never reaches the server; unwrapping is client-side.
    public var hasPassphrase: Bool
    /// When it was revoked, if it was.
    public var revokedAt: CapsuleTimestamp?

    public init(
        id: ShareID,
        opaqueID: String,
        secret: String,
        scope: ShareScope,
        expiresAt: CapsuleTimestamp? = nil,
        hasPassphrase: Bool = false,
        revokedAt: CapsuleTimestamp? = nil
    ) {
        self.id = id
        self.opaqueID = opaqueID
        self.secret = secret
        self.scope = scope
        self.expiresAt = expiresAt
        self.hasPassphrase = hasPassphrase
        self.revokedAt = revokedAt
    }

    /// Whether the link still resolves at the given instant.
    ///
    /// The serving endpoint is **fail-closed**; this is the same predicate it
    /// caches, so the UI and the server agree on what "live" means.
    public func isLive(at now: CapsuleTimestamp) -> Bool {
        guard revokedAt == nil else { return false }
        guard let expiresAt else { return true }
        return now < expiresAt
    }
}

// MARK: - LinkCaps

/// Per-link caps on a web-upload link, enforced **server-side at the no-key
/// layer** on every drop-session creation (*Web Upload — Security Contract*).
///
/// Enforced without any decryption key, which is why every cap is a byte count
/// or a count — nothing here requires the server to understand what was
/// uploaded.
public struct LinkCaps: Sendable, Equatable, Hashable {
    /// Expiry, or `nil` for none. Revocation still applies.
    public var expiresAt: CapsuleTimestamp?
    /// Cumulative byte cap across every drop on this link.
    public var maxTotalBytes: UInt64?
    /// How many files this link may deposit.
    public var maxFileCount: UInt32?
    /// Maximum single-file size.
    public var maxFileSize: UInt64?
    /// Whether the link dies after its first successful drop.
    public var singleUse: Bool

    public init(
        expiresAt: CapsuleTimestamp? = nil,
        maxTotalBytes: UInt64? = nil,
        maxFileCount: UInt32? = nil,
        maxFileSize: UInt64? = nil,
        singleUse: Bool = false
    ) {
        self.expiresAt = expiresAt
        self.maxTotalBytes = maxTotalBytes
        self.maxFileCount = maxFileCount
        self.maxFileSize = maxFileSize
        self.singleUse = singleUse
    }

    /// An uncapped link — every cap absent, multi-use.
    public static let unlimited = LinkCaps()

    /// Whether any cap at all is set.
    public var isUnlimited: Bool {
        self == .unlimited
    }
}

// MARK: - DropDescriptor

/// The descriptor a guest uploads beside the sealed ciphertext
/// (*Web Upload — Drop and Adoption Lifecycle*).
///
/// Deliberately **not** an asset manifest: no signatures, no album, no
/// provenance link. Its integrity is established only when a trusted client
/// decapsulates the file key and the AEAD tags verify. Until then every field
/// here is a claim by an unauthenticated stranger, and the UI must present it
/// as such.
public struct DropDescriptor: Sendable, Equatable, Hashable {
    /// The guest's asserted content type, from the link's pinned closed enum.
    public var contentType: ContentType
    /// Asserted plaintext byte length.
    public var plaintextSize: UInt64
    /// The encryption chunk size used for the seal.
    public var chunkSize: UInt32
    /// Content address of the ciphertext — the one field the server verifies,
    /// by recomputing it.
    public var ciphertextHash: BlobHash
    /// **Guest-supplied and unverified.** Advisory only: it is a string a
    /// stranger typed, so it must never be used as a filesystem path, and never
    /// rendered in a way that could pass for app chrome.
    public var suggestedFilename: String?

    public init(
        contentType: ContentType,
        plaintextSize: UInt64,
        chunkSize: UInt32,
        ciphertextHash: BlobHash,
        suggestedFilename: String? = nil
    ) {
        self.contentType = contentType
        self.plaintextSize = plaintextSize
        self.chunkSize = chunkSize
        self.ciphertextHash = ciphertextHash
        self.suggestedFilename = suggestedFilename
    }
}

// MARK: - PendingDrop

/// A drop awaiting review in the provisioning user's inbox.
///
/// One of the eight quarantine surfaces — see
/// ``QuarantineSurface/pendingDropAwaitingAdoption``. Nothing is applied without
/// an explicit human decision, and adoption happens **in place**: the already-
/// stored blob is reclassified from inbox to album asset, so the bulk bytes
/// incur no new quota.
public struct PendingDrop: Sendable, Equatable, Identifiable, Hashable {
    public var id: DropID
    /// Server-attested arrival time — the one timestamp here that is not
    /// self-asserted.
    public var receivedAt: CapsuleTimestamp
    /// The upload link it arrived through.
    public var viaLink: ShareID
    /// The guest's unsigned descriptor.
    public var descriptor: DropDescriptor

    public init(
        id: DropID,
        receivedAt: CapsuleTimestamp,
        viaLink: ShareID,
        descriptor: DropDescriptor
    ) {
        self.id = id
        self.receivedAt = receivedAt
        self.viaLink = viaLink
        self.descriptor = descriptor
    }

    /// The guest's suggested filename — **self-asserted and unverified**.
    ///
    /// Surfaced through the drop rather than reached for through the descriptor
    /// so that every call site meets this warning.
    public var suggestedFilename: String? {
        descriptor.suggestedFilename
    }
}
