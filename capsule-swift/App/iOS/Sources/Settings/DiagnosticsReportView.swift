import Foundation
import SwiftUI
import UIKit

/// A redacted diagnostics report ready to share, wrapped for `.sheet(item:)`.
struct DiagnosticsReport: Identifiable {
    let id = UUID()
    let data: Data
}

/// Presents the system share sheet for a diagnostics report.
///
/// The bundle is written to a temporary `.json` file so the share sheet offers
/// Mail / Files / AirDrop with a sensible filename. Works entirely on-device —
/// no backend required.
struct DiagnosticsReportView: UIViewControllerRepresentable {
    let report: DiagnosticsReport

    func makeUIViewController(context _: Context) -> UIActivityViewController {
        let url = FileManager.default.temporaryDirectory.appending(path: "capsule-diagnostics.json")
        try? report.data.write(to: url, options: .atomic)
        return UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}
