import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import Foundation

// MARK: - MockPalette

/// The colour every derived asset is built around.
///
/// One derivation, two readers: ``MockLibrary/lqip(derivationIndex:)`` puts it
/// in ``Lqip/dominantColor``, and ``MockThumbnailRenderer`` paints its gradient
/// around it. That is not tidiness — it is the degrade ladder working. The
/// bottom rung is a colour, drawn with no decode at all, and if the placeholder
/// colour and the thumbnail disagreed then every tile in the grid would flash a
/// different colour as it loaded.
public enum MockPalette {
    /// The asset's hue, in `[0, 1)`. Spread by a hash rather than by index, so
    /// a screenful of adjacent tiles is not a single colour ramp.
    public static func hue(seed: UInt64, derivationIndex: Int) -> Double {
        MockHash.fraction(MockHash.value(seed: seed, index: derivationIndex, salt: .colour))
    }

    /// The dominant colour: a mid-saturation, mid-value tone, which is what a
    /// photograph's average actually looks like. Fully saturated primaries would
    /// make the mock library look like a colour picker.
    public static func dominantColour(seed: UInt64, derivationIndex: Int) -> CapsuleDomain.RGBColor {
        let hueValue = hue(seed: seed, derivationIndex: derivationIndex)
        let spread = MockHash.value(seed: seed, index: derivationIndex, salt: .colour, sub: 1)
        let saturation = 0.28 + MockHash.fraction(spread) * 0.34
        let brightness = 0.42 + MockHash.fraction(MockHash.mix(spread)) * 0.36
        let components = rgb(hue: hueValue, saturation: saturation, brightness: brightness)
        return CapsuleDomain.RGBColor(
            red: UInt8(components.red * 255),
            green: UInt8(components.green * 255),
            blue: UInt8(components.blue * 255)
        )
    }

    /// The gradient's far end — the same hue rotated a little and darkened, the
    /// way light actually falls across a frame.
    public static func shadowColour(seed: UInt64, derivationIndex: Int) -> CapsuleDomain.RGBColor {
        let hueValue = (hue(seed: seed, derivationIndex: derivationIndex) + 0.08).truncatingRemainder(dividingBy: 1)
        let spread = MockHash.value(seed: seed, index: derivationIndex, salt: .colour, sub: 2)
        let saturation = 0.34 + MockHash.fraction(spread) * 0.3
        let components = rgb(hue: hueValue, saturation: saturation, brightness: 0.22)
        return CapsuleDomain.RGBColor(
            red: UInt8(components.red * 255),
            green: UInt8(components.green * 255),
            blue: UInt8(components.blue * 255)
        )
    }

    /// Normalised colour components, before quantisation to the sidecar's
    /// `[u8; 3]`.
    struct Components: Sendable, Equatable {
        var red: Double
        var green: Double
        var blue: Double
    }

    /// HSB → RGB, written out rather than routed through a platform colour type
    /// because this module may not import one.
    static func rgb(hue: Double, saturation: Double, brightness: Double) -> Components {
        let sector = hue * 6
        let index = Int(sector) % 6
        let fraction = sector - Double(Int(sector))
        let dim = brightness * (1 - saturation)
        let falling = brightness * (1 - saturation * fraction)
        let rising = brightness * (1 - saturation * (1 - fraction))
        switch index {
        case 0: return Components(red: brightness, green: rising, blue: dim)
        case 1: return Components(red: falling, green: brightness, blue: dim)
        case 2: return Components(red: dim, green: brightness, blue: rising)
        case 3: return Components(red: dim, green: falling, blue: brightness)
        case 4: return Components(red: rising, green: dim, blue: brightness)
        default: return Components(red: brightness, green: dim, blue: falling)
        }
    }
}

// MARK: - MockThumbnail

/// A rendered thumbnail as a **value**.
///
/// Deliberately not a `CGImage` or a `PlatformImage`: neither is `Sendable`, and
/// a thumbnail crosses from the renderer's actor to whatever is drawing it.
/// Carrying the pixels and rebuilding the image on the consumer's side keeps the
/// whole path concurrency-safe with no `@unchecked` anywhere.
public struct MockThumbnail: Sendable, Equatable {
    /// Pixel width.
    public let width: Int
    /// Pixel height.
    public let height: Int
    /// 8-bit RGBA, alpha last and premultiplied, tightly packed row-major.
    public let pixels: Data
    /// The colour the LQIP reports for the same asset.
    public let dominantColor: CapsuleDomain.RGBColor

    public init(width: Int, height: Int, pixels: Data, dominantColor: CapsuleDomain.RGBColor) {
        self.width = width
        self.height = height
        self.pixels = pixels
        self.dominantColor = dominantColor
    }

    /// Bytes held, for the cache's cost accounting.
    public var byteCount: Int { pixels.count }

    /// Rebuild a drawable image. `nil` only if the pixel buffer is the wrong
    /// size for the declared dimensions, which would be a defect here.
    public func makeImage() -> CGImage? {
        let bytesPerRow = width * 4
        guard width > 0, height > 0, pixels.count == bytesPerRow * height else { return nil }
        guard let provider = CGDataProvider(data: pixels as CFData) else { return nil }
        return CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: bytesPerRow,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        )
    }

    /// The platform's bitmap image type, for a view that wants one.
    ///
    /// Goes through ``PlatformImage/fromCGImage(_:scale:)`` in
    /// `CapsuleFoundation` rather than importing UIKit or AppKit, which this
    /// module is not allowed to do and does not need to.
    public func makePlatformImage(scale: CGFloat = 1) -> PlatformImage? {
        makeImage().map { PlatformImage.fromCGImage($0, scale: scale) }
    }
}
