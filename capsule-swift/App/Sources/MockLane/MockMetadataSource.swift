import AssetKit
import CapsuleFoundation
import Foundation
import ImagePipeline

/// The info panel's metadata in the mock lane, derived from the asset itself.
///
/// The mock library is synthetic and has no files behind it, so there is no EXIF
/// to read — and `ViewerMediaLoader` reaches PhotoKit through an identifier only
/// a system asset carries. Without this the panel rendered a date and nothing
/// else, which reads as "this photo has no metadata" rather than as "this lane
/// cannot see any".
///
/// Every value is a **pure function of the asset's identifier**, the same
/// discipline `CapsuleMock` applies to the rest of the library: the same photo
/// shows the same camera and the same file size on every launch and on every
/// machine, so a screenshot is comparable and a UI test can assert on it. None
/// of it is random.
///
/// It lives in the app target for the same structural reason
/// ``PortBackedThumbnailProvider`` does: the protocol is declared in
/// `ImagePipeline`, which already depends on `AssetKit`, so the conformance
/// cannot live in `AssetKit` without making the two mutually dependent.
struct MockMetadataSource: AssetMetadataSource {
    /// Where the photo was taken, read from the library rather than invented.
    ///
    /// The coordinate is the one field here that is *not* derived: the mock
    /// library already stores one per asset and the Places screen renders it, so
    /// deriving a second from the seed would put the same photo in two places
    /// and neither would be wrong-looking on its own.
    private let locations: any AssetLocationSource

    init(locations: any AssetLocationSource) {
        self.locations = locations
    }

    /// The cameras the synthetic library was shot on.
    private static let cameras: [(make: String, model: String)] = [
        ("Apple", "iPhone 17 Pro Max"),
        ("Apple", "iPhone 15"),
        ("Fujifilm", "X-T5"),
        ("Sony", "α7R V"),
        ("Canon", "EOS R6"),
    ]

    /// A lens as the panel names it, with optics that match the name — a 24 mm
    /// "Ultra Wide" would be a lie the panel tells about itself.
    private struct Lens {
        let name: String
        let focalLength: Double
        let aperture: Double
    }

    /// Lens configurations as a phone camera reports them.
    private static let lenses = [
        Lens(name: "Main Camera", focalLength: 24, aperture: 1.78),
        Lens(name: "Ultra Wide", focalLength: 13, aperture: 2.2),
        Lens(name: "Telephoto", focalLength: 77, aperture: 2.8),
        Lens(name: "Main Camera", focalLength: 48, aperture: 1.78),
    ]

    private static let photoCodecs = ["HEIF", "JPEG", "ProRAW"]
    private static let videoCodecs = ["HEVC", "H.264"]

    func metadata(for asset: Asset) async -> AssetExifMetadata? {
        // PhotoKit assets are not this lane's to describe: the real loader reads
        // their actual EXIF, and answering here would overwrite fact with
        // fixture.
        guard !asset.isFromPhotoKit else { return nil }

        let seed = Self.seed(for: asset.id)
        var metadata = AssetExifMetadata()

        let camera = Self.cameras[Int(seed % UInt64(Self.cameras.count))]
        metadata.cameraMake = camera.make
        metadata.cameraModel = camera.model

        let lens = Self.lenses[Int((seed >> 8) % UInt64(Self.lenses.count))]
        metadata.lensName = lens.name
        metadata.focalLength = lens.focalLength
        metadata.aperture = lens.aperture
        metadata.isoSpeed = Self.isoLadder[Int((seed >> 16) % UInt64(Self.isoLadder.count))]
        metadata.shutterSpeed = Self.shutterLadder[
            Int((seed >> 24) % UInt64(Self.shutterLadder.count))
        ]

        let isVideo = asset.mediaType != .photo
        let codec = isVideo
            ? Self.videoCodecs[Int((seed >> 32) % UInt64(Self.videoCodecs.count))]
            : Self.photoCodecs[Int((seed >> 32) % UInt64(Self.photoCodecs.count))]
        metadata.codec = codec
        metadata.originalFilename = Self.filename(seed: seed, codec: codec)
        metadata.byteCount = Self.byteCount(seed: seed, asset: asset, isVideo: isVideo)
        if isVideo {
            metadata.frameRate = Self.frameRates[Int((seed >> 40) % UInt64(Self.frameRates.count))]
            // Only some clips are HDR, so the panel has to render both cases.
            if seed.isMultiple(of: 3) { metadata.hdrFormat = .dolbyVision }
        }

        if let coordinate = await locations.location(for: asset.id) {
            metadata.latitude = coordinate.latitude
            metadata.longitude = coordinate.longitude
        }
        return metadata
    }

    // MARK: Ladders

    /// Real ISO stops, not a continuous range: a panel reading "ISO 437" would
    /// be a number no camera ever wrote.
    private static let isoLadder = [50, 64, 100, 200, 400, 800, 1600, 3200]
    /// Shutter speeds as fractions of a second, on the same principle.
    private static let shutterLadder = [1.0 / 4000, 1.0 / 1000, 1.0 / 250, 1.0 / 60, 1.0 / 15, 0.5]
    private static let frameRates = [24.0, 25.0, 30.0, 60.0, 120.0]

    // MARK: Derivation

    /// A stable 64-bit seed for an asset, mixed so that neighbouring
    /// identifiers do not produce neighbouring choices.
    ///
    /// **Not `hashValue`.** Swift seeds `Hashable` per process, so the same
    /// photo would show a different camera on every launch — which would break
    /// the determinism this whole type exists to provide, and would do it
    /// invisibly, since any single run looks perfectly consistent. FNV-1a over
    /// the identifier's bytes is stable across launches and machines, and
    /// splitmix64's finalizer then spreads it — the same mixer `CapsuleMock`
    /// derives the rest of the library with.
    private static func seed(for id: AssetID) -> UInt64 {
        let text: String = switch id {
        case let .photoKit(localIdentifier): localIdentifier
        case let .managed(uuid): uuid
        }
        var value: UInt64 = 0xCBF2_9CE4_8422_2325
        for byte in text.utf8 {
            value ^= UInt64(byte)
            value &*= 0x0000_0100_0000_01B3
        }
        value ^= value >> 30
        value &*= 0xBF58_476D_1CE4_E5B9
        value ^= value >> 27
        value &*= 0x94D0_49BB_1331_11EB
        value ^= value >> 31
        return value
    }

    /// A filename whose extension agrees with the codec beside it.
    ///
    /// Derived from the codec rather than from the media kind, because the panel
    /// shows both on the same card: an `IMG_9811.HEIC` labelled ProRAW is the
    /// sort of detail that makes a synthetic library look like a bug.
    private static func filename(seed: UInt64, codec: String) -> String {
        let number = 1000 + Int(seed % 8999)
        let ext = switch codec {
        case "HEVC", "H.264": "MOV"
        case "ProRAW": "DNG"
        case "JPEG": "JPG"
        default: "HEIC"
        }
        return "IMG_\(number).\(ext)"
    }

    /// A plausible size for the asset's pixel count and kind.
    ///
    /// Derived from the dimensions rather than picked, so a 4K video is larger
    /// than a snapshot and the panel's three file facts agree with one another.
    private static func byteCount(seed: UInt64, asset: Asset, isVideo: Bool) -> Int64 {
        let pixels = max(1, asset.pixelWidth * asset.pixelHeight)
        if isVideo {
            // Bitrate × duration, at a rate that varies a little per asset.
            let megabitsPerSecond = 40.0 + Double(seed % 40)
            let seconds = max(1, asset.duration)
            return Int64(megabitsPerSecond * 125000 * seconds)
        }
        // Roughly a byte per two pixels for HEIF, varied per asset.
        let bytesPerPixel = 0.35 + Double(seed % 30) / 100
        return Int64(Double(pixels) * bytesPerPixel)
    }
}
