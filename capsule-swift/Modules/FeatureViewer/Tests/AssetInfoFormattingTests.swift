import Foundation
import ImagePipeline
import Testing

@testable import FeatureViewer

@Suite("Info panel formatting")
struct AssetInfoFormattingTests {
    // MARK: File size

    /// Decimal, not binary. A camera that says 35.7 MB and a panel that says
    /// 34.0 MB are describing the same file and the reader has no way to know
    /// that.
    @Test("a file size uses the decimal convention a camera reports")
    func fileSizeIsDecimal() {
        let text = AssetInfoFormatting.fileSize(35700000)
        #expect(text?.contains("35.7") == true)
        #expect(text?.contains("MB") == true)
    }

    @Test("a zero or negative size has no row at all")
    func emptySizeIsAbsent() {
        #expect(AssetInfoFormatting.fileSize(0) == nil)
        #expect(AssetInfoFormatting.fileSize(-1) == nil)
    }

    // MARK: Resolution

    /// The assertion that catches an orientation bug. A 2160 × 3840 portrait
    /// clip is 4K; matching on width would call it nothing at all.
    @Test("resolution is named from the short edge, so orientation cannot change it")
    func resolutionIsOrientationIndependent() {
        #expect(AssetInfoFormatting.resolutionClass(width: 3840, height: 2160) == "4K")
        #expect(AssetInfoFormatting.resolutionClass(width: 2160, height: 3840) == "4K")
    }

    @Test(
        "each resolution band gets its name",
        arguments: [
            (7680, 4320, "8K"), (3840, 2160, "4K"), (2560, 1440, "QHD"),
            (1920, 1080, "HD"), (1280, 720, "HD"),
        ]
    )
    func resolutionBands(width: Int, height: Int, expected: String) {
        #expect(AssetInfoFormatting.resolutionClass(width: width, height: height) == expected)
    }

    @Test("a resolution too small to have a name gets none")
    func smallResolutionIsUnnamed() {
        #expect(AssetInfoFormatting.resolutionClass(width: 640, height: 480) == nil)
        #expect(AssetInfoFormatting.resolutionClass(width: 0, height: 0) == nil)
    }

    @Test("dimensions use the multiplication sign, not the letter x")
    func dimensionsUseMultiplicationSign() {
        #expect(AssetInfoFormatting.dimensions(width: 2160, height: 3840) == "2160 × 3840")
        #expect(AssetInfoFormatting.dimensions(width: 0, height: 100) == nil)
    }

    // MARK: Camera name

    /// Canon writes its make into its model, so a naive join says "Canon Canon
    /// EOS R6" — which is the kind of defect that ships because nobody on the
    /// team owns that camera.
    @Test("a make already inside the model is not repeated")
    func makeIsNotRepeated() {
        #expect(AssetInfoFormatting.cameraName(make: "Canon", model: "Canon EOS R6") == "Canon EOS R6")
        #expect(AssetInfoFormatting.cameraName(make: "Apple", model: "iPhone 17 Pro Max")
            == "Apple iPhone 17 Pro Max")
    }

    @Test("either half alone is still a name")
    func partialCameraNames() {
        #expect(AssetInfoFormatting.cameraName(make: "Apple", model: nil) == "Apple")
        #expect(AssetInfoFormatting.cameraName(make: nil, model: "X-T5") == "X-T5")
        #expect(AssetInfoFormatting.cameraName(make: nil, model: nil) == nil)
    }

    /// An EXIF field that is present but blank is absent, not a name made of
    /// spaces.
    @Test("a whitespace-only field is treated as missing")
    func blankFieldsAreMissing() {
        #expect(AssetInfoFormatting.cameraName(make: "   ", model: nil) == nil)
        #expect(AssetInfoFormatting.cameraName(make: "  ", model: "X-T5") == "X-T5")
    }

    // MARK: Lens

    @Test("the lens line reads the way a camera writes it")
    func lensLineIsComposed() {
        let line = AssetInfoFormatting.lensLine(name: "Main Camera", focalLength: 24, aperture: 1.78)
        #expect(line == "Main Camera — 24 mm ƒ1.78")
    }

    /// A whole-stop aperture drops its trailing zero, the way a lens barrel
    /// and Apple's panel both write it.
    @Test("an aperture keeps the digits it needs and no more")
    func aperturePrecision() {
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: nil, aperture: 1.78)
            == "ƒ1.78")
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: nil, aperture: 2.2)
            == "ƒ2.2")
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: nil, aperture: 2.0)
            == "ƒ2")
    }

    @Test("a lens line survives any part being unknown")
    func lensLineDegrades() {
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: 24, aperture: 1.78)
            == "24 mm ƒ1.78")
        #expect(AssetInfoFormatting.lensLine(name: "Ultra Wide", focalLength: nil, aperture: nil)
            == "Ultra Wide")
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: nil, aperture: nil) == nil)
    }

    /// Zero is what a scan or a synthetic image reports, and "0 mm ƒ0" is worse
    /// than saying nothing.
    @Test("zeroed optics are dropped rather than printed")
    func zeroOpticsAreDropped() {
        #expect(AssetInfoFormatting.lensLine(name: nil, focalLength: 0, aperture: 0) == nil)
    }

    // MARK: Shutter

    @Test("a fast shutter reads as a fraction and a slow one as seconds")
    func shutterSpeedSpelling() {
        #expect(AssetInfoFormatting.shutterSpeed(1.0 / 250) == "1/250")
        #expect(AssetInfoFormatting.shutterSpeed(0.5) == "1/2")
        #expect(AssetInfoFormatting.shutterSpeed(2) == "2.0s")
        #expect(AssetInfoFormatting.shutterSpeed(0) == nil)
    }

    // MARK: Duration and frame rate

    /// Zero-padded under an hour, because this sits beside a frame rate in a row
    /// of figures and an unpadded minute makes the row jump as it ticks.
    @Test("a duration is zero-padded under an hour and grows an hour field over one")
    func durationSpelling() {
        #expect(AssetInfoFormatting.duration(11) == "00:11")
        #expect(AssetInfoFormatting.duration(62) == "01:02")
        #expect(AssetInfoFormatting.duration(3723) == "1:02:03")
        #expect(AssetInfoFormatting.duration(0) == nil)
    }

    @Test("a whole frame rate loses its decimals and a broadcast one keeps them")
    func frameRateSpelling() {
        #expect(AssetInfoFormatting.frameRate(30) == "30 FPS")
        #expect(AssetInfoFormatting.frameRate(29.97) == "29.97 FPS")
        #expect(AssetInfoFormatting.frameRate(0) == nil)
    }

    // MARK: Composition

    @Test("the file line joins what is known and omits what is not")
    func fileLineComposes() {
        #expect(AssetInfoFormatting.fileLine(
            resolutionClass: "4K", dimensions: "2160 × 3840", fileSize: "35.7 MB"
        ) == "4K • 2160 × 3840 • 35.7 MB")

        #expect(AssetInfoFormatting.fileLine(
            resolutionClass: nil, dimensions: "800 × 600", fileSize: nil
        ) == "800 × 600")

        #expect(AssetInfoFormatting.fileLine(
            resolutionClass: nil, dimensions: nil, fileSize: nil
        ) == nil)
    }

    @Test("every HDR format has a name spelled the way its owner spells it")
    func hdrNames() {
        #expect(AssetInfoFormatting.hdrName(.dolbyVision) == "Dolby Vision")
        #expect(AssetInfoFormatting.hdrName(.hdr10) == "HDR10")
        #expect(AssetInfoFormatting.hdrName(.hlg) == "HLG")
        // Every case is covered, so a new one fails to compile rather than
        // silently rendering nothing.
        #expect(HDRFormat.allCases.count == 3)
    }
}
