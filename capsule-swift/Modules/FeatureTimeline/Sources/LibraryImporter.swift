import AssetKit
import CapsuleFoundation
import Foundation
import ManagedStore
import Observation

/// Coordinates importing picked photos into the Capsule-managed library and
/// publishes the flow's state to the timeline screen.
///
/// After an import it refreshes the ``ManagedProvider`` so the new assets
/// appear in the unified timeline, then cleans up the picker's temporary files.
@MainActor
@Observable
public final class LibraryImporter {
    /// Whether an import is currently running.
    public private(set) var isImporting = false
    /// The most recent import's result; cleared once its summary is dismissed.
    public var lastResult: ImportResult?
    /// Bound to the photo-picker sheet's presentation.
    public var isPickerPresented = false

    private let importService: ImportService
    private let managedProvider: ManagedProvider

    public init(importService: ImportService, managedProvider: ManagedProvider) {
        self.importService = importService
        self.managedProvider = managedProvider
    }

    /// Present the system photo picker.
    public func presentPicker() {
        isPickerPresented = true
    }

    /// Import the picker's output, then refresh the managed timeline.
    public func importPicked(_ sources: [ImportSource]) async {
        guard !sources.isEmpty else { return }
        isImporting = true
        CapsuleLog.managedStore.info("importing \(sources.count) picked file(s)")
        let result = await importService.importAssets(from: sources)
        await managedProvider.refresh()
        for source in sources {
            try? FileManager.default.removeItem(at: source.url)
        }
        isImporting = false
        lastResult = result
    }
}
