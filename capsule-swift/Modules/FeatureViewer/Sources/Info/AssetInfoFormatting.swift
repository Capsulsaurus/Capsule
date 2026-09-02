import AssetKit
import Foundation
import ImagePipeline

/// Turns the info panel's raw metadata into the strings it prints.
///
/// Free of SwiftUI so every rule below is a unit test rather than a squint at a
/// simulator, and free of catalog keys because none of these are translatable
/// prose: they are numbers, units, and format names that read the same in every
/// locale a camera writes them in. The one thing that *is* localized — the
/// capture date — is formatted by SwiftUI at the call site, where the reader's
/// calendar is in scope.
enum AssetInfoFormatting {
    // MARK: File size

    /// A file size the way a photo app writes one: "35.7 MB".
    ///
    /// `ByteCountFormatter`'s decimal convention (1 MB = 1 000 000 bytes),
    /// because that is what a camera's own display and Apple's Photos both use.
    /// Reporting 34.0 MiB where the camera said 35.7 MB is not more accurate to
    /// a reader, it is just different.
    static func fileSize(_ bytes: Int64) -> String? {
        guard bytes > 0 else { return nil }
        let formatter = ByteCountFormatter()
        formatter.countStyle = .decimal
        formatter.allowsNonnumericFormatting = false
        return formatter.string(fromByteCount: bytes)
    }

    // MARK: Resolution

    /// The marketing name for a resolution — "4K", "HD" — when one applies.
    ///
    /// Matched on the *smaller* dimension so a portrait clip and a landscape one
    /// of the same recording get the same name. A 2160 × 3840 video is 4K held
    /// either way up, and calling it "HD" because its width is small would be
    /// wrong in the way that is hardest to notice.
    static func resolutionClass(width: Int, height: Int) -> String? {
        let shortEdge = min(width, height)
        guard shortEdge > 0 else { return nil }
        return switch shortEdge {
        case 4320...: "8K"
        case 2160 ..< 4320: "4K"
        case 1440 ..< 2160: "QHD"
        case 1080 ..< 1440: "HD"
        case 720 ..< 1080: "HD"
        default: nil
        }
    }

    /// Pixel dimensions with the multiplication sign, not the letter x.
    static func dimensions(width: Int, height: Int) -> String? {
        guard width > 0, height > 0 else { return nil }
        return "\(width) × \(height)"
    }

    /// Megapixels, to one decimal place.
    static func megapixels(width: Int, height: Int) -> String? {
        guard width > 0, height > 0 else { return nil }
        return String(format: "%.1f MP", Double(width * height) / 1000000)
    }

    // MARK: Camera

    /// The lens line a camera writes: "Main Camera — 24 mm ƒ1.78".
    ///
    /// Assembled from whichever parts are present, so a body with no lens name
    /// still reports its focal length. Returns `nil` rather than an empty string
    /// when nothing is known, so the caller drops the row instead of drawing a
    /// blank one.
    static func lensLine(
        name: String?,
        focalLength: Double?,
        aperture: Double?
    ) -> String? {
        var trailing: [String] = []
        if let focalLength, focalLength > 0 {
            trailing.append(String(format: "%.0f mm", focalLength))
        }
        if let aperture, aperture > 0 {
            // The photographic ƒ, and no slash: this is how a lens barrel and
            // Apple's own panel write it. `%g` rather than a fixed precision so
            // ƒ1.78 keeps both digits while ƒ2.0 loses the zero it does not
            // need — a fixed `%.2f` prints "ƒ1.78" and "ƒ2.00", and a `%.2g`
            // rounds the first to "ƒ1.8".
            trailing.append(String(format: "ƒ%g", aperture))
        }
        let detail = trailing.joined(separator: " ")
        switch (name, detail.isEmpty) {
        case let (name?, false): return "\(name) — \(detail)"
        case let (name?, true): return name
        case (nil, false): return detail
        case (nil, true): return nil
        }
    }

    /// A shutter speed as a photographer reads it: "1/250" or "0.5s".
    static func shutterSpeed(_ seconds: Double) -> String? {
        guard seconds > 0 else { return nil }
        if seconds >= 1 { return String(format: "%.1fs", seconds) }
        return "1/\(Int((1 / seconds).rounded()))"
    }

    // MARK: Time

    /// A duration as a clock reads it: "00:11", "1:02:03".
    ///
    /// Zero-padded minutes even under an hour, because this sits next to a frame
    /// rate in a row of figures and "0:11" would jump as the seconds tick.
    static func duration(_ seconds: TimeInterval) -> String? {
        guard seconds > 0 else { return nil }
        let total = Int(seconds.rounded())
        let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60)
        return hours > 0
            ? String(format: "%d:%02d:%02d", hours, minutes, secs)
            : String(format: "%02d:%02d", minutes, secs)
    }

    /// A frame rate without a pointless decimal: "30 FPS", "29.97 FPS".
    static func frameRate(_ rate: Double) -> String? {
        guard rate > 0 else { return nil }
        let rounded = rate.rounded()
        return abs(rate - rounded) < 0.01
            ? String(format: "%.0f FPS", rounded)
            : String(format: "%.2f FPS", rate)
    }

    // MARK: HDR

    /// The name of an HDR encoding, as its owner spells it.
    static func hdrName(_ format: HDRFormat) -> String {
        switch format {
        case .hdr10: "HDR10"
        case .dolbyVision: "Dolby Vision"
        case .hlg: "HLG"
        }
    }

    // MARK: Composition

    /// The camera's own name: "Apple iPhone 17 Pro Max".
    ///
    /// Makes are commonly repeated inside the model — Canon writes "Canon EOS
    /// R6" — so a naive join produces "Canon Canon EOS R6".
    static func cameraName(make: String?, model: String?) -> String? {
        switch (make?.trimmed, model?.trimmed) {
        case let (make?, model?):
            model.localizedCaseInsensitiveContains(make) ? model : "\(make) \(model)"
        case let (make?, nil): make
        case let (nil, model?): model
        case (nil, nil): nil
        }
    }

    /// The file line: "4K • 2160 × 3840 • 35.7 MB", minus whatever is unknown.
    static func fileLine(
        resolutionClass: String?,
        dimensions: String?,
        fileSize: String?
    ) -> String? {
        let parts = [resolutionClass, dimensions, fileSize].compactMap(\.self)
        return parts.isEmpty ? nil : parts.joined(separator: " • ")
    }
}

private extension String {
    /// `nil` rather than an empty string, so a blank EXIF field is absent rather
    /// than present-and-empty.
    var trimmed: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
