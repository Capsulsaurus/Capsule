import Foundation
import Testing

import CapsuleCatalog

@Suite("SidecarCodec — CBOR round-trip")
struct SidecarCodecTests {
    private func makeSidecar() -> CatalogSidecar {
        CatalogSidecar(
            version: 1,
            uuid: "01956ef3-0000-7000-8000-000000000001",
            assetType: "photo",
            originalFilename: "IMG_1234.HEIC",
            importTimestamp: 1_720_000_000,
            modifiedTimestamp: 1_720_000_000,
            hashSHA256: String(repeating: "a", count: 64),
            fileSize: 2_400_000,
            importerVersion: "capsule-ios/0.1.0",
            rawshiftVersion: "0.0.0"
        )
    }

    @Test("a minimal sidecar round-trips through CBOR")
    func minimalRoundTrip() throws {
        let sidecar = makeSidecar()
        let bytes = try SidecarCodec.encode(sidecar)
        #expect(!bytes.isEmpty)
        #expect(try SidecarCodec.decode(bytes) == sidecar)
    }

    @Test("a fully-populated sidecar with a stack hint round-trips")
    func fullRoundTrip() throws {
        var sidecar = makeSidecar()
        sidecar.captureTimestamp = 1_719_990_000
        sidecar.captureUTC = 1_719_986_400
        sidecar.captureTimezone = "America/New_York"
        sidecar.captureTimezoneSource = "offset_exif"
        sidecar.width = 4032
        sidecar.height = 3024
        sidecar.durationMillis = nil
        sidecar.tags = ["trip", "2024"]
        sidecar.rating = 4
        sidecar.cameraMake = "Apple"
        sidecar.cameraModel = "iPhone 16 Pro"
        sidecar.gpsLatitude = 40.7128
        sidecar.gpsLongitude = -74.0060
        sidecar.stackHint = CatalogStackHint(
            detectionKey: "apple-content-identifier-XYZ",
            detectionMethod: "content_identifier",
            memberRole: "primary",
            stackType: "live_photo"
        )

        #expect(try SidecarCodec.decode(SidecarCodec.encode(sidecar)) == sidecar)
    }

    @Test("an unrecognised asset type is rejected at encode time")
    func invalidEnumRejected() {
        var sidecar = makeSidecar()
        sidecar.assetType = "not_a_real_type"
        #expect(throws: CatalogError.self) {
            try SidecarCodec.encode(sidecar)
        }
    }

    @Test("decoding malformed bytes throws rather than crashing")
    func malformedBytesRejected() {
        #expect(throws: CatalogError.self) {
            try SidecarCodec.decode(Data([0xFF, 0x00, 0x13, 0x37]))
        }
    }
}
