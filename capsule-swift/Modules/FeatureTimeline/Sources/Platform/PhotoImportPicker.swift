import Foundation
import ManagedStore
import SwiftUI
import UniformTypeIdentifiers

public extension View {
    /// Present the platform's photo-import chooser, handing back the picked
    /// files as ``ImportSource`` values.
    ///
    /// The two platforms disagree about what "import photos" means, and the
    /// honest answer differs with them: on iOS the system photo picker is the
    /// only way to reach the photo library without a permission prompt, while
    /// on macOS the library is an ordinary folder and the open panel is both
    /// available and the gesture users expect — it also yields real filenames,
    /// which the iOS picker only sometimes does.
    func photoImportPicker(
        isPresented: Binding<Bool>,
        onPicked: @escaping @Sendable ([ImportSource]) -> Void
    ) -> some View {
        modifier(PhotoImportPicker(isPresented: isPresented, onPicked: onPicked))
    }
}

/// The platform-specific half of ``SwiftUI/View/photoImportPicker(isPresented:onPicked:)``.
private struct PhotoImportPicker: ViewModifier {
    @Binding var isPresented: Bool
    let onPicked: @Sendable ([ImportSource]) -> Void

    func body(content: Content) -> some View {
        #if os(iOS)
            content.sheet(isPresented: $isPresented) {
                PhotoPickerView(onPicked: onPicked)
                    .ignoresSafeArea()
            }
        #else
            content.fileImporter(
                isPresented: $isPresented,
                allowedContentTypes: [.image],
                allowsMultipleSelection: true
            ) { result in
                guard case let .success(urls) = result else { return }
                onPicked(Self.importSources(from: urls))
            }
        #endif
    }

    #if !os(iOS)
        /// Copy each chosen file into a temporary file we own, mirroring what the
        /// iOS picker's coordinator does.
        ///
        /// The copy is not an optimisation — the open panel hands back a
        /// security-scoped URL that stops being readable the moment this call
        /// returns, so the import pipeline must be given a file inside our own
        /// container.
        private static func importSources(from urls: [URL]) -> [ImportSource] {
            urls.compactMap { url in
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                let destination = FileManager.default.temporaryDirectory
                    .appending(path: UUID().uuidString)
                    .appendingPathExtension(url.pathExtension)
                do {
                    try FileManager.default.copyItem(at: url, to: destination)
                } catch {
                    return nil
                }
                return ImportSource(url: destination, originalFilename: url.lastPathComponent)
            }
        }
    #endif
}
