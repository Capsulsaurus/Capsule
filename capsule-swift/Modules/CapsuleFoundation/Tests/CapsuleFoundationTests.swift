import Foundation
import Testing

@testable import CapsuleFoundation

@Suite("CapsuleFoundation value types")
struct CapsuleFoundationTests {
    @Test("MediaType raw values are stable across encodings")
    func mediaTypeRawValues() {
        #expect(MediaType.photo.rawValue == "photo")
        #expect(MediaType.video.rawValue == "video")
        #expect(MediaType.livePhoto.rawValue == "livePhoto")
        #expect(MediaType.allCases.count == 3)
    }

    @Test("MediaType.isMotion distinguishes playable media")
    func mediaTypeMotion() {
        #expect(MediaType.photo.isMotion == false)
        #expect(MediaType.video.isMotion)
        #expect(MediaType.livePhoto.isMotion)
    }

    @Test("AssetID round-trips through Codable for both sources")
    func assetIDCodable() throws {
        let ids: [AssetID] = [
            .photoKit(localIdentifier: "B84E8479-475C-4727-A4A4-B77AA9980897/L0/001"),
            .managed(uuid: "01956ef3-0000-7000-8000-000000000001"),
        ]
        for id in ids {
            let encoded = try JSONEncoder().encode(id)
            let decoded = try JSONDecoder().decode(AssetID.self, from: encoded)
            #expect(decoded == id)
        }
    }

    @Test("AssetID source predicates select the owning provider")
    func assetIDSourcePredicates() {
        let photoKit = AssetID.photoKit(localIdentifier: "x")
        let managed = AssetID.managed(uuid: "y")
        #expect(photoKit.isPhotoKit)
        #expect(photoKit.isManaged == false)
        #expect(managed.isManaged)
        #expect(managed.isPhotoKit == false)
    }

    @Test("AssetID is Hashable and distinguishes sources sharing a key")
    func assetIDHashing() {
        let photoKit = AssetID.photoKit(localIdentifier: "shared")
        let managed = AssetID.managed(uuid: "shared")
        #expect(photoKit != managed)
        #expect(Set([photoKit, managed, photoKit]).count == 2)
    }
}
