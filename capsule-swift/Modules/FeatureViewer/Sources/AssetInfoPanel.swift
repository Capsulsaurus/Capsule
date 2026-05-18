import AssetKit
import ImagePipeline
import MapKit
import SwiftUI

/// The swipe-up info panel — capture details, camera/exposure metadata read
/// from the asset's EXIF, and a location map.
struct AssetInfoPanel: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    @Environment(\.dismiss) private var dismiss
    @State private var metadata = AssetExifMetadata()

    var body: some View {
        NavigationStack {
            List {
                captureSection
                if !metadata.isEmpty {
                    cameraSection
                }
                if let coordinate {
                    locationSection(coordinate)
                }
            }
            .navigationTitle("Info")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .task(id: asset.id) {
            metadata = await mediaLoader.metadata(for: asset)
        }
    }

    private var captureSection: some View {
        Section("Capture") {
            row("Type", asset.mediaType.displayName)
            row("Date", asset.captureDate.formatted(date: .long, time: .shortened))
            if asset.pixelWidth > 0, asset.pixelHeight > 0 {
                row("Dimensions", "\(asset.pixelWidth) × \(asset.pixelHeight)")
                row("Resolution", megapixels)
            }
            if asset.mediaType != .photo, asset.duration > 0 {
                row("Duration", durationText)
            }
        }
    }

    private var cameraSection: some View {
        Section("Camera") {
            if let make = metadata.cameraMake { row("Make", make) }
            if let model = metadata.cameraModel { row("Model", model) }
            if let lens = metadata.lensModel { row("Lens", lens) }
            if let iso = metadata.isoSpeed { row("ISO", "\(iso)") }
            if let aperture = metadata.aperture {
                row("Aperture", String(format: "ƒ/%.1f", aperture))
            }
            if let shutter = metadata.shutterSpeed {
                row("Shutter", shutterText(shutter))
            }
            if let focal = metadata.focalLength {
                row("Focal Length", String(format: "%.0f mm", focal))
            }
        }
    }

    private func locationSection(_ coordinate: CLLocationCoordinate2D) -> some View {
        Section("Location") {
            Map(initialPosition: .region(MKCoordinateRegion(
                center: coordinate,
                latitudinalMeters: 800,
                longitudinalMeters: 800
            ))) {
                Marker("", coordinate: coordinate)
            }
            .frame(height: 160)
            .allowsHitTesting(false)
            .listRowInsets(EdgeInsets())
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).foregroundStyle(.secondary)
            Spacer(minLength: 16)
            Text(value).multilineTextAlignment(.trailing)
        }
    }

    private var coordinate: CLLocationCoordinate2D? {
        guard let latitude = metadata.latitude, let longitude = metadata.longitude else { return nil }
        return CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }

    private var megapixels: String {
        String(format: "%.1f MP", Double(asset.pixelWidth * asset.pixelHeight) / 1_000_000)
    }

    private var durationText: String {
        let total = Int(asset.duration.rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }

    private func shutterText(_ seconds: Double) -> String {
        guard seconds > 0 else { return "—" }
        if seconds >= 1 {
            return String(format: "%.1fs", seconds)
        }
        return "1/\(Int((1 / seconds).rounded()))"
    }
}
