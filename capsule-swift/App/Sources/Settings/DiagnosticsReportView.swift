import CapsuleUI
import Foundation
import SwiftUI

/// A redacted diagnostics report ready to share, wrapped for `.sheet(item:)`.
struct DiagnosticsReport: Identifiable {
    let id = UUID()
    let data: Data
}

/// Offers a redacted diagnostics report to the system share sheet.
///
/// The bundle is written to a temporary `.json` file so the share sheet offers
/// Mail / Files / AirDrop with a sensible filename. Works entirely on-device —
/// no backend required.
///
/// Built on `ShareLink` rather than a bridged `UIActivityViewController`:
/// `ShareLink` is the SwiftUI-native share affordance and is the same API on
/// iOS and macOS, so this screen needs no platform branch at all.
struct DiagnosticsReportView: View {
    let report: DiagnosticsReport

    @Environment(\.dismiss) private var dismiss
    @State private var fileURL: URL?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if let fileURL {
                        ShareLink(item: fileURL)
                    } else {
                        ProgressView()
                    }
                } footer: {
                    Text("app.settings.report.footer")
                }
            }
            .navigationTitle("app.diagnostics.report.title")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("app.common.done") { dismiss() }
                }
            }
        }
        .capsuleSheetDetents()
        .task { fileURL = Self.write(report) }
    }

    /// Write the bundle where the share sheet can read it back by URL.
    ///
    /// A fixed filename, so a second export overwrites the first rather than
    /// littering the temporary directory with one file per attempt.
    private static func write(_ report: DiagnosticsReport) -> URL? {
        let url = FileManager.default.temporaryDirectory.appending(path: "capsule-diagnostics.json")
        do {
            try report.data.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }
}
