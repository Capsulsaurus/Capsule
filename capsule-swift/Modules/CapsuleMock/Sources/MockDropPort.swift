import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - DropPort

/// Inbound web-upload links and the drops they collect.
///
/// A drop is unauthenticated content from a stranger. Nothing about it is
/// trustworthy until a trusted client decapsulates the file key and the AEAD
/// tags verify, which is why the review step is mandatory and every descriptor
/// field is presented as a claim.
extension MockSharingStore: DropPort {
    /// Provision an upload link with its caps.
    ///
    /// Caps are enforced **server-side at the no-key layer** on every drop
    /// session, so a leaked link cannot push storage past the owner's hard
    /// limit — the enforcement does not depend on the server understanding what
    /// was uploaded, which it cannot.
    public func createUploadLink(
        destination: AlbumID,
        caps: LinkCaps,
        passphrase: String?
    ) async throws -> ShareLink {
        guard await store.container(destination) != nil else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: unknown destination album")
        }
        let link = MockSharingSeed.link(
            seed: configuration.seed,
            ordinal: nextLinkOrdinal,
            scope: .album(destination),
            expiresAt: caps.expiresAt,
            hasPassphrase: passphrase?.isEmpty == false
        )
        addLink(link)
        return link
    }

    public func revokeUploadLink(_ identifier: ShareID) async throws {
        revoke(identifier)
    }

    /// The inbox of drops awaiting review.
    ///
    /// One of the eight quarantine surfaces: nothing is applied without an
    /// explicit decision.
    public func pendingDrops(offset: Int, limit: Int) async throws -> Page<PendingDrop> {
        let request = PageRequest(offset: offset, limit: limit)
        let items = pending
        return Page(
            items: MockQueryEngine.window(items, request: request),
            request: request,
            totalCount: items.count
        )
    }

    /// Adopt a drop into an album.
    ///
    /// Adoption happens **in place** — the already-stored blob is reclassified
    /// from inbox to album asset — so the bulk bytes incur no new quota and only
    /// the small metadata and provenance writes are charged. The returned
    /// identifier is a real, resolvable asset, because a mock that returned a
    /// dangling id would make the "open what you just adopted" flow untestable.
    public func adopt(_ identifier: DropID, into albumID: AlbumID) async throws -> AssetID {
        guard let drop = pending.first(where: { $0.id == identifier }) else {
            throw CapsuleError(code: .dropNotInInbox, detail: "CapsuleMock: drop is not in the inbox")
        }
        guard await store.container(albumID) != nil else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: unknown destination album")
        }
        guard store.library.assetCount > 0 else {
            throw CapsuleError(code: .dropNotInInbox, detail: "CapsuleMock: the library has no slot to adopt into")
        }
        // The adopted asset takes an existing derived slot and is moved into the
        // destination, which models reclassification-in-place: no new bytes, one
        // metadata write.
        let index = Int(MockHash.value(
            seed: configuration.seed,
            index: drop.descriptor.plaintextSize.hashValue,
            salt: .identity,
            sub: 4242
        ) % UInt64(max(1, store.library.assetCount)))
        let assetID = store.library.identifier(at: index)
        try await store.move([assetID], to: albumID)
        removeDrop(identifier)
        await dropChanges.send(())
        return assetID
    }

    /// Discard a drop. Its bytes are freed at the next collection.
    public func discard(_ identifier: DropID) async throws {
        removeDrop(identifier)
        await dropChanges.send(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        dropChanges.subscribe()
    }
}
