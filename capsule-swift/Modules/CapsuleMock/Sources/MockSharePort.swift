import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - SharePort

/// Outbound share links.
///
/// Issuing a link hands out **decryption material**, not a server-side
/// permission: the recipient decrypts with the fragment secret, and the server
/// never holds anything it can open. Two consequences shape everything here — a
/// link cannot be un-shared retroactively for someone who already opened it, so
/// revocation stops *future* resolution; and the returned value carries live
/// secrets that must never be logged.
extension MockSharingStore: SharePort {
    public func links() async throws -> [ShareLink] {
        links
    }

    /// Issue a link.
    ///
    /// An album-wide scope hands over the AMK for every epoch the album's
    /// history policy covers, which is a categorically larger thing than one
    /// file key — ``ShareScope/isAlbumWide`` exists so a confirmation sheet can
    /// make that unmistakable rather than describing both as "sharing".
    public func createLink(
        scope: ShareScope,
        expiresAt: CapsuleTimestamp?,
        passphrase: String?
    ) async throws -> ShareLink {
        let link = MockSharingSeed.link(
            seed: configuration.seed,
            ordinal: nextLinkOrdinal,
            scope: scope,
            expiresAt: expiresAt,
            hasPassphrase: passphrase?.isEmpty == false
        )
        addLink(link)
        return link
    }

    /// Revoke a link.
    ///
    /// The serving endpoint is fail-closed, so it refuses within its cache
    /// window rather than serving until a TTL lapses — which is why revocation
    /// is worth offering at all on material that has already left the building.
    public func revokeLink(_ identifier: ShareID) async throws {
        revoke(identifier)
    }

    /// Open a link this user received.
    ///
    /// The passphrase is unwrapped **client-side** and never reaches the server;
    /// a missing one is a specific, recoverable error rather than a generic
    /// failure, because the user can supply it.
    public func openLink(
        opaqueID: String,
        secret: String,
        passphrase: String?
    ) async throws -> Page<LibraryAsset> {
        guard let link = links.first(where: { $0.opaqueID == opaqueID }) else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: no such link")
        }
        guard link.isLive(at: configuration.clock.now) else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: link is revoked or expired")
        }
        guard link.secret == secret else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: fragment secret does not decrypt")
        }
        if link.hasPassphrase, passphrase?.isEmpty != false {
            throw CapsuleError(
                code: .dropPassphraseRequired,
                detail: "CapsuleMock: this link carries an Argon2id layer"
            )
        }
        return try await resolve(scope: link.scope)
    }

    private func resolve(scope: ShareScope) async throws -> Page<LibraryAsset> {
        switch scope {
        case let .asset(assetID):
            let asset = try await store.asset(for: assetID)
            let request = PageRequest(offset: 0, limit: 1)
            return Page(items: asset.map { [$0] } ?? [], request: request, totalCount: asset == nil ? 0 : 1)
        case let .album(albumID):
            return try await store.assets(
                matching: TimelineQuery(albumID: albumID),
                offset: 0,
                limit: PageRequest.defaultLimit
            )
        }
    }
}
