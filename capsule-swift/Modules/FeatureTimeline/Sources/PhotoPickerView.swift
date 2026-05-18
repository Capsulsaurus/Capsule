import ManagedStore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// A `PHPickerViewController` bridged into SwiftUI for photo import.
///
/// The system picker needs no photo-library permission. On selection each
/// picked item is copied to a temporary file and surfaced as an
/// ``ImportSource`` for the import pipeline.
struct PhotoPickerView: UIViewControllerRepresentable {
    let onPicked: @Sendable ([ImportSource]) -> Void

    func makeUIViewController(context: Context) -> PHPickerViewController {
        var configuration = PHPickerConfiguration()
        configuration.filter = .images
        configuration.selectionLimit = 0
        let controller = PHPickerViewController(configuration: configuration)
        controller.delegate = context.coordinator
        return controller
    }

    func updateUIViewController(_: PHPickerViewController, context _: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onPicked: onPicked)
    }

    final class Coordinator: NSObject, PHPickerViewControllerDelegate {
        private let onPicked: @Sendable ([ImportSource]) -> Void

        init(onPicked: @escaping @Sendable ([ImportSource]) -> Void) {
            self.onPicked = onPicked
        }

        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            picker.dismiss(animated: true)
            let onPicked = onPicked
            Task {
                var sources: [ImportSource] = []
                for result in results {
                    if let source = await Self.loadSource(from: result) {
                        sources.append(source)
                    }
                }
                onPicked(sources)
            }
        }

        /// Copy a picked item to a temporary file we own.
        private static func loadSource(from result: PHPickerResult) async -> ImportSource? {
            let provider = result.itemProvider
            let displayName = provider.suggestedName ?? "Imported Photo"
            return await withCheckedContinuation { continuation in
                provider.loadFileRepresentation(for: .image, openInPlace: false) { url, _, _ in
                    guard let url else {
                        continuation.resume(returning: nil)
                        return
                    }
                    let destination = FileManager.default.temporaryDirectory
                        .appending(path: UUID().uuidString)
                        .appendingPathExtension(url.pathExtension)
                    do {
                        try FileManager.default.copyItem(at: url, to: destination)
                        continuation.resume(returning: ImportSource(
                            url: destination,
                            originalFilename: displayName
                        ))
                    } catch {
                        continuation.resume(returning: nil)
                    }
                }
            }
        }
    }
}
