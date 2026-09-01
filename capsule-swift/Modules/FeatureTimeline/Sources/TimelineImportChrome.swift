import CapsuleUI
import ManagedStore
import SwiftUI

// The Library's *import* chrome: the permission wall, the progress scrim, and
// the sentence that reports what an import did.
//
// Separated from `TimelineRootView` because none of it is about drawing a grid,
// and because that view's body had grown past the length the lint allows.
// Components with explicit inputs rather than an extension: Swift's `private` is
// file-scoped, so an extension in another file cannot reach a view's `@State`,
// and widening a dozen properties to internal to work around that would be worse
// than passing two closures.

// MARK: - Permission

/// The wall shown when the system photo library has not been authorized.
struct LibraryPermissionPrompt: View {
    /// Opens the app's Settings page, when the platform offers one.
    let onOpenSettings: (URL) -> Void

    var body: some View {
        ContentUnavailableView {
            Label("app.timeline.permission.title", systemImage: "lock.fill")
        } description: {
            Text("app.timeline.permission.description")
        } actions: {
            if let settingsURL = PhotoLibrarySettings.url {
                Button("app.timeline.permission.open_settings") { onOpenSettings(settingsURL) }
            }
        }
    }
}

// MARK: - Progress

/// The scrim over the library while an import runs.
///
/// A dimming layer rather than glass: this one is over *content* rather than on
/// the control layer, and it is deliberately blocking — the point is that the
/// library is not interactive while its contents are changing underneath.
struct ImportProgressOverlay: View {
    var body: some View {
        ZStack {
            Color.black.opacity(0.3).ignoresSafeArea()
            ProgressView("app.timeline.importing")
                .padding(CapsuleTheme.Spacing.xLarge)
                .capsuleGlass(in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.medium))
        }
    }
}

// MARK: - Result

/// What an import did, as the sentence the completion alert shows.
///
/// Each clause is a catalog plural rather than an interpolated English sentence:
/// the count agreement is the translator's to make, and a language that inflects
/// the noun cannot be served by concatenating a number onto a fixed string.
enum ImportSummary {
    static func text(for result: ImportResult) -> String {
        var lines: [String] = []
        if result.importedCount > 0 {
            lines.append(String(
                localized: "app.timeline.import.imported",
                defaultValue: "\(result.importedCount) imported into Capsule."
            ))
        }
        if result.duplicateCount > 0 {
            lines.append(String(
                localized: "app.timeline.import.duplicates",
                defaultValue: "\(result.duplicateCount) already in your library."
            ))
        }
        if result.failureCount > 0 {
            lines.append(String(
                localized: "app.timeline.import.failed",
                defaultValue: "\(result.failureCount) couldn't be imported."
            ))
        }
        return lines.isEmpty
            ? String(localized: "app.timeline.import.nothing")
            : lines.joined(separator: "\n")
    }
}
