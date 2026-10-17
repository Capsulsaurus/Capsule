import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Seeding

/// The world's starting albums.
///
/// `static` rather than instance methods because an actor's synchronous
/// initializer runs outside the actor's isolation, so it cannot call isolated
/// members. Building the seed as values and assigning them is both what the
/// compiler requires and the clearer shape: seeding is a pure function of the
/// configuration.
enum MockAlbumSeed {
    /// Container albums.
    ///
    /// Ordinal 0 is the **default album**: nameless, undeletable while
    /// designated, and the destination an unfiled import lands in. It exists for
    /// every owner from first-device enrollment onward precisely so import
    /// always has somewhere to go, and modelling it as "the album with no name"
    /// rather than as an album called something is what keeps a destination
    /// picker honest.
    static func containers(configuration: MockConfiguration, library: MockLibrary) -> [ContainerAlbum] {
        let seed = configuration.seed
        let policy = AlbumPolicy(historyPolicy: .full, retentionDays: 30, protocolVersion: "2026-05-01")
        let owner = AlbumMember(handle: MockSidecarFactory.ownerHandle, role: .admin)
        return (0 ..< MockLibrary.albumCount).map { ordinal in
            ContainerAlbum(
                id: MockIdentifiers.albumID(seed: seed, ordinal: ordinal),
                name: ordinal == 0 ? nil : names[ordinal % names.count],
                coverAssetID: cover(library: library, ordinal: ordinal),
                count: 0,
                epoch: UInt32(3 + ordinal * 2),
                policy: ordinal == 4 ? cappedPolicy : policy,
                members: ordinal == 3 ? [owner] + sharedMembers : [owner],
                isDefault: ordinal == 0
            )
        }
    }

    /// Album names are user *data*, so they are drawn from the same token
    /// vocabulary the tags use rather than being written as display copy.
    private static let names = ["archive", "travel", "family", "client-work", "print", "film-scan"]

    /// One album is capped-history and pinned to an older protocol version, so
    /// the "this album cannot accept that schema" and "a joiner sees only recent
    /// epochs" surfaces are reachable.
    private static let cappedPolicy = AlbumPolicy(
        historyPolicy: .capped,
        retentionDays: 14,
        protocolVersion: "2025-11-01"
    )

    /// A shared album needs members with different capabilities, or the role
    /// controls have nothing to show.
    private static let sharedMembers = [
        AlbumMember(handle: "morgan@capsule.example", role: .write),
        AlbumMember(handle: "sam@other.example", role: .read),
    ]

    /// The first asset assigned to an album makes a plausible cover, and is
    /// derived rather than picked so it is stable across launches.
    private static func cover(library: MockLibrary, ordinal: Int) -> AssetID? {
        guard library.assetCount > 0 else { return nil }
        for index in 0 ..< min(library.assetCount, 4000)
            where library.albumOrdinal(derivationIndex: index) == ordinal {
            return library.identifier(at: index)
        }
        return library.identifier(at: 0)
    }
}

// MARK: - MockSmartAlbumSeed

enum MockSmartAlbumSeed {
    /// Four definitions covering four field classes, plus — under
    /// ``MockScenario/newerVersionState`` — one written against a grammar this
    /// build cannot evaluate.
    ///
    /// That last one is the point of the set. A definition ahead of this build
    /// must be **preserved verbatim and never evaluated**: evaluating it
    /// partially would show a different album here than on the device that
    /// wrote it, and stripping it would be the never-strip rule broken. So the
    /// mock ships one, and the UI has something to render as "created by a newer
    /// app version".
    static func definitions(configuration: MockConfiguration) -> [SmartAlbumDefinition] {
        var result = [
            definition(configuration, ordinal: 0, name: "rating-4-plus", predicate: .term(Term(
                field: .rating,
                operatorKind: .greaterThanOrEqual,
                operand: .number(4)
            ))),
            definition(configuration, ordinal: 1, name: "video", predicate: .term(Term(
                field: .mediaKind,
                operatorKind: .equalTo,
                operand: .enumerationValue(MediaKind.video.rawValue)
            ))),
            definition(configuration, ordinal: 2, name: "picks", predicate: .term(Term(
                field: .cull,
                operatorKind: .isValue,
                operand: .cullFlag(.pick)
            ))),
            definition(configuration, ordinal: 3, name: "located", predicate: .all([
                .term(Term(field: .gps, operatorKind: .exists, operand: .boolean(true))),
                .term(Term(field: .rating, operatorKind: .greaterThanOrEqual, operand: .number(1))),
            ])),
        ]
        guard configuration.hasSmartAlbumFromNewerGrammar else { return result }
        var ahead = definition(configuration, ordinal: 4, name: "future-grammar", predicate: .term(Term(
            field: .tagsUser,
            operatorKind: .containsAny,
            operand: .stringSet(["travel"])
        )))
        ahead.predicateSchema = SmartAlbumDefinition.currentPredicateSchema + 1
        result.append(ahead)
        return result
    }

    private static func definition(
        _ configuration: MockConfiguration,
        ordinal: Int,
        name: String,
        predicate: SmartAlbumPredicate
    ) -> SmartAlbumDefinition {
        SmartAlbumDefinition(
            smartAlbumID: MockIdentifiers.smartAlbumID(seed: configuration.seed, ordinal: ordinal),
            displayName: Lww(current: Stamped(
                value: name,
                timestamp: configuration.clock.offset(days: -30 - ordinal),
                author: MockTagIdentity.authoringDevice(seed: configuration.seed)
            )),
            predicate: predicate
        )
    }
}
