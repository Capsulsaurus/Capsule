import Foundation

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// One sample of every `Route` case, with the section that must own it.
///
/// A hand-maintained census rather than something derived, because `Route` has
/// associated values and so cannot be `CaseIterable`. It is the backbone of
/// three separate guarantees — `Codable` round-trip, section ownership, and the
/// unreachability of the `?? .library` fallback in `Route.owningSection` — so a
/// case added to the enum and forgotten here weakens all three at once.
///
/// > Important: adding a case to `Route` means adding a row here.
enum RouteFixtures {
    static let assetID = AssetID.managed(uuid: "0192f0c0-1111-7000-8000-000000000001")
    static let albumID = AlbumID.managed(uuid: "0192f0c0-2222-7000-8000-000000000002")
    static let photoKitAsset = AssetID.photoKit(localIdentifier: "ABC-123/L0/001")
    static let smartAlbumID = SmartAlbumID("0192f0c0-3333-7000-8000-000000000003")
    static let personID = PersonID("0192f0c0-4444-7000-8000-000000000004")
    static let importID = ImportID("0192f0c0-5555-7000-8000-000000000005")
    static let quarantineID = QuarantineID("0192f0c0-6666-7000-8000-000000000006")
    static let shareID = ShareID("0192f0c0-7777-7000-8000-000000000007")
    static let dropID = DropID("0192f0c0-8888-7000-8000-000000000008")

    static let region = MapRegion(
        minimumLatitude: 43.6,
        maximumLatitude: 43.8,
        minimumLongitude: -79.5,
        maximumLongitude: -79.2
    )

    /// A query with **every** facet populated, so a `Codable` bridge that drops
    /// a field fails the round-trip instead of passing on defaults.
    static let fullQuery = TimelineQuery(
        slice: .userHidden,
        albumID: albumID,
        mediaKind: .video,
        capturedAfter: CapsuleTimestamp(epochSeconds: 1700000000),
        capturedBefore: CapsuleTimestamp(epochSeconds: 1800000000),
        cull: .pick,
        minimumRating: 4,
        includeStackHidden: true
    )

    /// Every route case, paired with the section that must own it.
    static let census: [(route: Route, section: SidebarItem)] = [
        (.timeline(.all), .library),
        (.timeline(.years), .library),
        (.memories, .memories),
        (.duplicates, .duplicates),
        (.trash, .trash),
        (.hidden, .hidden),
        (.viewer(assetID, context: .album(albumID)), .albums),
        (.viewer(photoKitAsset, context: .timeline(.trash)), .trash),
        (.culling(.timeline(fullQuery)), .hidden),
        (.albums, .albums),
        (.album(albumID), .albums),
        (.albumMembers(albumID), .albums),
        (.albumPolicy(albumID), .albums),
        (.smartAlbum(smartAlbumID), .albums),
        (.smartAlbumEditor(smartAlbumID), .albums),
        (.smartAlbumEditor(nil), .albums),
        (.people, .people),
        (.person(personID), .people),
        (.places, .places),
        (.place(region), .places),
        (.search(.all, text: "sunset"), .search),
        (.search([.semantic, .people], text: nil), .search),
        (.transferCenter, .transfers),
        (.uploadDetail(assetID), .transfers),
        (.custodyReceipt(assetID), .transfers),
        (.imports, .imports),
        (.importSession(importID), .imports),
        (.quarantine, .quarantine),
        (.quarantineItem(quarantineID), .quarantine),
        (.shares, .shares),
        (.shareDetail(shareID), .shares),
        (.drops, .drops),
        (.drop(dropID), .drops),
        (.linkRedemption(.share, opaqueID: "abc123"), .shares),
        (.linkRedemption(.guestUpload, opaqueID: "def456"), .drops),
        (.devices, .devices),
        (.peers, .peers),
        (.federation, .federation),
        (.quota, .storage),
        (.storage, .storage),
        (.maintenance(nil), .storage),
        (.maintenance(.deepContentValidation), .storage),
        (.settings(.account), .settings),
        (.settings(.keysAndDevices), .settings),
        (.onboarding(.welcome), .settings),
    ]

    static var routes: [Route] { census.map(\.route) }
}
