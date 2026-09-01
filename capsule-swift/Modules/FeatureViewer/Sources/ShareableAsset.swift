import AssetKit
import CapsuleFoundation
import CoreGraphics
import CoreTransferable
import Foundation
import ImageIO
import ImagePipeline
import UniformTypeIdentifiers

/// One asset packaged for the system share sheet.
///
/// This is what lets the viewer and the timeline drop `UIActivityViewController`
/// — which has no macOS counterpart — for SwiftUI's `ShareLink`, which is
/// cross-platform. It is also strictly better behaviour: the full-resolution
/// image is decoded and encoded *inside* the transfer, when and only when the
/// user actually picks a destination, instead of eagerly loading every selected
/// asset into memory before the sheet can even open.
///
/// Encoding goes through ImageIO rather than a per-platform `jpegData`
/// equivalent, so there is one code path on both platforms.
public struct ShareableAsset: Transferable, Identifiable, Sendable {
    /// Pixel budget for the exported image. Matches what the viewer's old share
    /// path requested — large enough for print, small enough to mail.
    private static let exportPixelSize = CGSize(width: 3072, height: 3072)
    /// Quality of the exported JPEG. 0.9 is visually lossless at this size.
    private static let exportQuality = 0.9

    public var id: AssetID { asset.id }

    private let asset: Asset
    private let mediaLoader: ViewerMediaLoader

    public init(asset: Asset, mediaLoader: ViewerMediaLoader) {
        self.asset = asset
        self.mediaLoader = mediaLoader
    }

    /// A share-sheet preview title. The capture date rather than a translated
    /// noun, so this introduces no user-facing string of its own.
    public var previewTitle: String {
        asset.captureDate.formatted(date: .abbreviated, time: .shortened)
    }

    public static var transferRepresentation: some TransferRepresentation {
        DataRepresentation(exportedContentType: .jpeg) { shareable in
            try await shareable.exportedJPEG()
        }
        .suggestedFileName { $0.suggestedFileName }
    }

    /// A stable, sortable filename derived from the capture instant. Not a
    /// user-facing string — it is a filename, and deliberately locale-free.
    private var suggestedFileName: String {
        let stamp = asset.captureDate.formatted(
            Date.ISO8601FormatStyle(dateSeparator: .omitted, timeSeparator: .omitted, timeZone: .gmt)
        )
        return "capsule-\(stamp).jpg"
    }

    /// Decode the asset at share resolution and re-encode it as JPEG.
    ///
    /// `@MainActor` because ``ViewerMediaLoader`` is main-actor confined and
    /// `PlatformImage` is not `Sendable` on macOS: keeping the whole decode →
    /// encode hop on the main actor means only the resulting `Data` — which is
    /// `Sendable` — ever crosses an isolation boundary.
    @MainActor
    private func exportedJPEG() async throws -> Data {
        guard let image = await mediaLoader.fullImage(for: asset, targetSize: Self.exportPixelSize),
              let cgImage = image.platformCGImage
        else {
            throw ShareableAssetError.imageUnavailable
        }
        return try Self.encodeJPEG(cgImage)
    }

    private static func encodeJPEG(_ cgImage: CGImage) throws -> Data {
        let buffer = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            buffer, UTType.jpeg.identifier as CFString, 1, nil
        ) else {
            throw ShareableAssetError.encodingFailed
        }
        CGImageDestinationAddImage(destination, cgImage, [
            kCGImageDestinationLossyCompressionQuality: exportQuality,
        ] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else {
            throw ShareableAssetError.encodingFailed
        }
        return buffer as Data
    }
}

/// Why an asset could not be handed to the share sheet.
public enum ShareableAssetError: Error {
    /// The asset's full-resolution image could not be loaded — the original is
    /// missing, or is in iCloud and unreachable.
    case imageUnavailable
    /// ImageIO refused to produce a JPEG for the decoded image.
    case encodingFailed
}
