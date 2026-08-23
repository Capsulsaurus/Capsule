import CapsuleDomain
import CapsuleMock
import SwiftUI

// MARK: - ImportScanProgressView

/// **Step 2 — reading the source.**
///
/// Determinate where a total exists and indeterminate where it does not, because
/// the two source families genuinely differ: a photo library knows its count
/// before the first item is read, a directory walk does not. A bar drawn against
/// a guessed total would have to jump backwards, which reads as a fault.
public struct ImportScanProgressView: View {
    @State private var model: ImportScanProgressModel
    private let onScanned: (@MainActor (ImportScan) -> Void)?
    private let onCancelled: (@MainActor () -> Void)?

    public init(
        model: ImportScanProgressModel,
        onScanned: (@MainActor (ImportScan) -> Void)? = nil,
        onCancelled: (@MainActor () -> Void)? = nil
    ) {
        _model = State(initialValue: model)
        self.onScanned = onScanned
        self.onCancelled = onCancelled
    }

    public init(
        scope: ImportScope,
        environment: ImportEnvironment,
        onScanned: (@MainActor (ImportScan) -> Void)? = nil,
        onCancelled: (@MainActor () -> Void)? = nil
    ) {
        self.init(
            model: ImportScanProgressModel(scope: scope, environment: environment),
            onScanned: onScanned,
            onCancelled: onCancelled
        )
    }

    public var body: some View {
        ImportScreen(
            titleKey: "ios.import.scan.title",
            phase: model.phase,
            emptyTitleKey: "ios.import.scan.empty.title",
            emptyDescriptionKey: "ios.import.scan.empty.description",
            emptySymbol: "magnifyingglass",
            retry: { await model.start() },
            content: { scanList }
        )
        .task { await model.start() }
    }

    private var scanList: some View {
        List {
            progressSection
            sourceSection
            unreadableSection
            actionSection
        }
    }

    // MARK: Sections

    private var progressSection: some View {
        Section {
            ImportScanIndicator(progress: model.progress, state: model.state)
            ImportValueRow(labelKey: "ios.import.scan.found", value: ImportFormat.count(model.progress.itemsFound))
            ImportValueRow(labelKey: "ios.import.scan.bytes", value: ImportFormat.bytes(model.progress.bytesFound))
            currentLocatorRow
        } header: {
            Text("ios.import.scan.header")
        }
    }

    @ViewBuilder
    private var currentLocatorRow: some View {
        if let locator = model.progress.currentLocator, model.state == .scanning {
            ImportValueRow(labelKey: "ios.import.scan.current", value: ImportFormat.leaf(locator))
        }
    }

    private var sourceSection: some View {
        Section {
            ImportValueRow(
                labelKey: "ios.import.scan.source",
                value: String(localized: String.LocalizationValue(model.source.sourceKind.importTitleKey))
            )
            ImportValueRow(labelKey: "ios.import.scan.location", value: ImportFormat.leaf(model.source.locator))
        }
    }

    /// Unreadable locators are surfaced, never silently skipped: a permissions
    /// problem is a different thing from an unsupported format, and a user who
    /// scans four hundred files and sees three hundred and eighty is owed the
    /// reason for the other twenty.
    @ViewBuilder
    private var unreadableSection: some View {
        if !model.unreadableLocators.isEmpty {
            Section {
                ForEach(model.unreadableLocators, id: \.self) { locator in
                    Label(ImportFormat.leaf(locator), systemImage: "lock.slash")
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("ios.import.scan.unreadable")
            } footer: {
                Text("ios.import.scan.unreadable.footer")
            }
        }
    }

    private var actionSection: some View {
        Section {
            if model.isCancellable {
                Button("ios.import.scan.cancel", role: .cancel) {
                    model.cancel()
                    onCancelled?()
                }
            }
            continueButton
            cancelledNote
        }
    }

    @ViewBuilder
    private var continueButton: some View {
        if let scan = model.scan, model.canContinue {
            Button("ios.import.scan.continue") { onScanned?(scan) }
                .buttonStyle(.borderedProminent)
        }
    }

    @ViewBuilder
    private var cancelledNote: some View {
        if model.state == .cancelled {
            ImportNote(textKey: "ios.import.scan.cancelled.description")
        }
    }
}

// MARK: - ImportScanIndicator

/// The progress indicator, determinate only when a total honestly exists.
private struct ImportScanIndicator: View {
    let progress: ImportScanProgress
    let state: ImportScanProgressModel.State

    var body: some View {
        indicator
            .accessibilityLabel(Text("ios.import.scan.header"))
            .accessibilityValue(Text(verbatim: ImportFormat.count(progress.itemsFound)))
    }

    @ViewBuilder
    private var indicator: some View {
        if state == .finished {
            ImportStatusLabel(titleKey: "ios.import.scan.complete", tone: .positive)
        } else if state == .cancelled {
            ImportStatusLabel(titleKey: "ios.import.scan.cancelled.title", tone: .caution)
        } else if let fraction = progress.fraction {
            ProgressView(value: fraction) {
                Text("ios.import.scan.scanning")
            }
        } else {
            ProgressView {
                Text("ios.import.scan.scanning")
            }
            .progressViewStyle(.linear)
        }
    }
}

// MARK: - Previews

#Preview("Scan — photo library (determinate)") {
    let environment = ImportEnvironment.preview(.healthy)
    return NavigationStack {
        ImportScanProgressView(scope: PreviewScopes.cameraRoll, environment: environment)
    }
}

#Preview("Scan — archive (indeterminate)") {
    let environment = ImportEnvironment.preview(.healthy)
    return NavigationStack {
        ImportScanProgressView(scope: PreviewScopes.takeout, environment: environment)
    }
}

#Preview("Scan — offline") {
    let environment = ImportEnvironment.preview(.offline)
    return NavigationStack {
        ImportScanProgressView(scope: PreviewScopes.cameraRoll, environment: environment)
    }
}

// MARK: - PreviewScopes

/// Fixed scopes for the previews.
///
/// Hand-built rather than fetched from the port: a `#Preview` body cannot await,
/// and a scope is three fields plus an id the mock derives anyway.
enum PreviewScopes {
    static let cameraRoll = ImportScope(
        scopeID: "preview-camera-roll",
        platform: PlatformTag(rawValue: "ios"),
        sourceKind: .cameraRoll,
        locator: "photokit://camera-roll"
    )

    static let takeout = ImportScope(
        scopeID: "preview-takeout",
        platform: PlatformTag(rawValue: "ios"),
        sourceKind: .takeoutArchive,
        locator: "file:///Downloads/takeout-20260812.zip"
    )
}
