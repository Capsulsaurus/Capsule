import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - SharePort

/// Outbound share links.
///
/// Issuing a link hands out **decryption material**, not a server-side
/// permission: the recipient decrypts with the fragment secret, and the server
/// never holds anything it can open. Two consequences the protocol is shaped
/// around — a link cannot be "un-shared" retroactively for someone who already
/// opened it, so revocation stops *future* resolution; and the returned
/// ``ShareLink`` contains live secrets that must never be logged.
public protocol SharePort: Sendable {
    /// Every link this user has issued that is still on record, revoked ones
    /// included so the history is auditable.
    ///
    /// Maps to `sharing.list_links`.
    func links() async throws -> [ShareLink]

    /// Issue a link.
    ///
    /// The returned value carries the fragment secret, which **never leaves the
    /// client** — the server holds only material wrapped around it.
    ///
    /// Maps to `sharing.create_link`.
    func createLink(
        scope: ShareScope,
        expiresAt: CapsuleTimestamp?,
        passphrase: String?
    ) async throws -> ShareLink

    /// Revoke a link. The serving endpoint is fail-closed, so it refuses within
    /// its cache window rather than serving until a TTL lapses.
    ///
    /// Maps to `sharing.revoke_link`.
    func revokeLink(_ id: ShareID) async throws

    /// Open a link this user received.
    ///
    /// - Throws: ``CapsuleError`` with `error.drop.passphrase_required` when the
    ///   link carries an Argon2id layer and no passphrase was supplied. The
    ///   passphrase is unwrapped **client-side** and never reaches the server.
    ///
    /// Maps to `sharing.open_link`.
    func openLink(
        opaqueID: String,
        secret: String,
        passphrase: String?
    ) async throws -> Page<LibraryAsset>
}

// MARK: - DropPort

/// Inbound web-upload links and the drops they collect.
///
/// A drop is unauthenticated content from a stranger. Nothing about it is
/// trustworthy until a trusted client decapsulates the file key and the AEAD
/// tags verify — which is why the review step is mandatory and every descriptor
/// field is presented as a claim.
public protocol DropPort: Sendable {
    /// Provision an upload link with its caps.
    ///
    /// Caps are enforced **server-side at the no-key layer** on every drop
    /// session, so a leaked link cannot push storage past the owner's hard
    /// limit.
    ///
    /// Maps to `drop.create_link`.
    func createUploadLink(
        destination: AlbumID,
        caps: LinkCaps,
        passphrase: String?
    ) async throws -> ShareLink

    /// Revoke an upload link.
    ///
    /// Maps to `drop.revoke_link`.
    func revokeUploadLink(_ id: ShareID) async throws

    /// The inbox of drops awaiting review.
    ///
    /// One of the eight quarantine surfaces: nothing is applied without an
    /// explicit decision.
    ///
    /// Maps to `drop.list_pending`.
    func pendingDrops(offset: Int, limit: Int) async throws -> Page<PendingDrop>

    /// Adopt a drop into an album.
    ///
    /// Adoption happens **in place** — the already-stored blob is reclassified
    /// from inbox to album asset — so the bulk bytes incur no new quota; only
    /// the small metadata and provenance writes are charged.
    ///
    /// Maps to `drop.adopt`.
    func adopt(_ id: DropID, into albumID: AlbumID) async throws -> AssetID

    /// Discard a drop. Its bytes are freed at the next collection.
    ///
    /// Maps to `drop.discard`.
    func discard(_ id: DropID) async throws

    /// A stream that fires when the inbox changes.
    func changes() -> AsyncStream<Void>
}
