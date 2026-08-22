import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockSharingStore

/// Outbound share links and the inbound drop inbox.
///
/// Both hand cryptographic material across a trust boundary, in opposite
/// directions: a share link hands out **decryption material** rather than a
/// server-side permission, and a drop brings in bytes from an unauthenticated
/// stranger. One actor, because the same link record backs both an outbound
/// share and an inbound upload link, and revoking one must revoke the other.
public actor MockSharingStore {
    nonisolated let store: MockLibraryStore
    nonisolated let configuration: MockConfiguration

    private var issuedLinks: [ShareLink]
    private var drops: [PendingDrop]
    private var adoptedDrops: Set<DropID> = []

    nonisolated let dropChanges = ChangeBroadcaster<Void>()

    public init(store: MockLibraryStore, configuration: MockConfiguration) {
        self.store = store
        self.configuration = configuration
        issuedLinks = MockSharingSeed.links(configuration: configuration)
        drops = MockSharingSeed.drops(configuration: configuration)
    }

    // MARK: State

    var links: [ShareLink] { issuedLinks }
    var pending: [PendingDrop] { drops.filter { !adoptedDrops.contains($0.id) } }

    func addLink(_ link: ShareLink) { issuedLinks.append(link) }

    func revoke(_ identifier: ShareID) {
        issuedLinks = issuedLinks.map { link in
            guard link.id == identifier, link.revokedAt == nil else { return link }
            var revoked = link
            revoked.revokedAt = configuration.clock.now
            return revoked
        }
    }

    func removeDrop(_ identifier: DropID) { adoptedDrops.insert(identifier) }

    /// The next ordinal for a newly minted link, so identifiers never collide
    /// with the seeded ones.
    var nextLinkOrdinal: Int { 700 + issuedLinks.count }
}

// MARK: - MockSharingSeed

enum MockSharingSeed {
    /// Issued links: one live album share, one expired, one revoked, one
    /// passphrase-wrapped upload link.
    ///
    /// Revoked ones stay on record because the history is auditable — and
    /// because a link cannot be un-shared retroactively for someone who already
    /// opened it, so the record is the only account of what was handed out.
    static func links(configuration: MockConfiguration) -> [ShareLink] {
        let seed = configuration.seed
        let clock = configuration.clock
        let album = MockIdentifiers.albumID(seed: seed, ordinal: 1)
        return [
            link(seed: seed, ordinal: 0, scope: .album(album), expiresAt: clock.offset(days: 12)),
            link(seed: seed, ordinal: 1, scope: .album(album), expiresAt: clock.offset(days: -3)),
            link(
                seed: seed,
                ordinal: 2,
                scope: .album(MockIdentifiers.albumID(seed: seed, ordinal: 2)),
                expiresAt: nil,
                revokedAt: clock.offset(days: -9)
            ),
            link(
                seed: seed,
                ordinal: 3,
                scope: .album(MockIdentifiers.albumID(seed: seed, ordinal: 3)),
                expiresAt: clock.offset(days: 30),
                hasPassphrase: true
            ),
        ]
    }

    static func link(
        seed: UInt64,
        ordinal: Int,
        scope: ShareScope,
        expiresAt: CapsuleTimestamp?,
        hasPassphrase: Bool = false,
        revokedAt: CapsuleTimestamp? = nil
    ) -> ShareLink {
        ShareLink(
            id: MockIdentifiers.shareID(seed: seed, ordinal: ordinal),
            opaqueID: MockIdentifiers.opaqueLinkID(seed: seed, ordinal: ordinal),
            secret: MockIdentifiers.linkSecret(seed: seed, ordinal: ordinal),
            scope: scope,
            expiresAt: expiresAt,
            hasPassphrase: hasPassphrase,
            revokedAt: revokedAt
        )
    }

    /// The drop inbox.
    ///
    /// Every descriptor field is a **claim by an unauthenticated stranger** —
    /// no signatures, no album, no provenance link — so the review step is
    /// mandatory. The suggested filenames are deliberately awkward, because a
    /// guest-supplied string must never be used as a path or rendered where it
    /// could pass for app chrome.
    static func drops(configuration: MockConfiguration) -> [PendingDrop] {
        let seed = configuration.seed
        let clock = configuration.clock
        let names = ["holiday.HEIC", "../../etc/passwd", "Settings — Capsule.png", nil]
        return names.enumerated().map { ordinal, name in
            PendingDrop(
                id: MockIdentifiers.dropID(seed: seed, ordinal: ordinal),
                receivedAt: clock.offset(days: -ordinal - 1),
                viaLink: MockIdentifiers.shareID(seed: seed, ordinal: 3),
                descriptor: DropDescriptor(
                    contentType: ordinal == 2 ? .png : .heic,
                    plaintextSize: UInt64(2_400_000 + ordinal * 1_100_000),
                    chunkSize: 1 << 20,
                    ciphertextHash: MockIdentifiers.blobHash(seed: seed, ordinal: 9000 + ordinal),
                    suggestedFilename: name
                )
            )
        }
    }
}
