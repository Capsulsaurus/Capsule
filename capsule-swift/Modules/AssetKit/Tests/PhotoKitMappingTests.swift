import Photos
import Testing

@testable import AssetKit

@Suite("PhotoKit authorization mapping")
struct PhotoKitMappingTests {
    @Test("each PHAuthorizationStatus maps to the source-agnostic status")
    func authorizationMapping() {
        #expect(AssetAuthorizationStatus(photoKit: .notDetermined) == .notDetermined)
        #expect(AssetAuthorizationStatus(photoKit: .restricted) == .restricted)
        #expect(AssetAuthorizationStatus(photoKit: .denied) == .denied)
        #expect(AssetAuthorizationStatus(photoKit: .authorized) == .authorized)
        #expect(AssetAuthorizationStatus(photoKit: .limited) == .limited)
    }
}
