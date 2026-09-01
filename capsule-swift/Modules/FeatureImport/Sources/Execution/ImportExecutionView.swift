import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ImportExecutionView

/// **Step 4 — the run.**
///
/// The list is virtualized and the rows are built on demand: `List` over an index
/// range asks the model for one item at a time, and the model keeps state only
/// for the items that have actually moved. A hundred-thousand-item run therefore
/// costs a few hundred dictionary entries rather than a hundred thousand row
/// structs.
public struct ImportExecutionView: View {
    @State private var model: ImportExecutionModel
    private let onFinished: (@MainActor (ImportSummary) -> Void)?

    public init(
        model: ImportExecutionModel,
        onFinished: (@MainActor (ImportSummary) -> Void)? = nil
    ) {
        _model = State(initialValue: model)
        self.onFinished = onFinished
    }

    public init(
        plan: ImportPlan,
        environment: ImportEnvironment,
        onFinished: (@MainActor (ImportSummary) -> Void)? = nil
    ) {
        self.init(model: ImportExecutionModel(plan: plan, environment: environment), onFinished: onFinished)
    }

    public var body: some View {
        ImportScreen(
            titleKey: "app.import.run.title",
            phase: model.phase,
            emptyTitleKey: "app.import.run.empty.title",
            emptyDescriptionKey: "app.import.run.empty.description",
            emptySymbol: "square.and.arrow.down",
            retry: { await model.run() },
            content: { runBody }
        )
        .task { await run() }
    }

    private func run() async {
        await model.run()
        if let summary = model.summary { onFinished?(summary) }
    }

    private var runBody: some View {
        List {
            headerSection
            itemsSection
        }
        .listStyle(.plain)
        .safeAreaInset(edge: .top) { progressBar }
        // The rows dissolve into the bar instead of being sheared off by it.
        .capsuleScrollEdgeEffect(for: .top)
    }

    // MARK: Chrome

    private var progressBar: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            ProgressView(value: model.fraction)
                .accessibilityLabel(Text("app.import.run.title"))
                .accessibilityValue(Text(verbatim: ImportFormat.percent(model.fraction)))
            Text(verbatim: progressText)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .monospacedDigit()
        }
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.vertical, CapsuleTheme.Spacing.small)
        .capsuleGlass(in: Rectangle())
    }

    private var progressText: String {
        String(
            format: String(localized: "app.import.run.progress"),
            ImportFormat.count(model.completedCount),
            ImportFormat.count(model.itemCount)
        )
    }

    private var headerSection: some View {
        Section {
            stateRow
            failureRow
            actionRow
        }
    }

    @ViewBuilder
    private var stateRow: some View {
        switch model.state {
        case .finished:
            ImportStatusLabel(titleKey: "app.import.run.done.title", tone: .positive)
        case .cancelled:
            ImportStatusLabel(titleKey: "app.import.run.cancelled.title", tone: .caution)
        case .running, .idle:
            EmptyView()
        }
    }

    @ViewBuilder
    private var failureRow: some View {
        if model.hasRetryableFailures {
            ImportValueRow(labelKey: "app.import.run.failures", value: ImportFormat.count(model.failedCount))
        }
    }

    @ViewBuilder
    private var actionRow: some View {
        if model.isCancellable {
            Button("app.import.run.cancel", role: .cancel) {
                Task { await model.cancel() }
            }
        }
        if model.hasRetryableFailures {
            Button("app.import.run.retry_all") {
                Task { await model.retryAll() }
            }
        }
        if model.state == .cancelled {
            ImportNote(textKey: "app.import.run.cancelled.description")
        }
    }

    // MARK: Items

    /// `List` over the index range, so only the visible rows are ever built.
    private var itemsSection: some View {
        Section {
            ForEach(model.itemIndices, id: \.self) { index in
                ImportRunRow(item: model.item(at: index), isRetrying: model.isRetrying(index)) {
                    Task { await model.retry(index) }
                }
            }
        } header: {
            Text("app.import.run.items.header")
        }
    }
}

// MARK: - ImportRunRow

/// One item's row: its name, its stage, and — when it failed — the localized
/// reason plus a retry.
///
/// The failure message comes from the code's own catalog key, never from the
/// English `detail` on ``CapsuleError``: that field is a diagnostic for support
/// bundles, and putting it on screen is a localisation bug rather than a helpful
/// extra.
private struct ImportRunRow: View {
    let item: ImportRunItem
    let isRetrying: Bool
    let retry: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            text
            Spacer(minLength: CapsuleTheme.Spacing.small)
            trailing
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(verbatim: ImportFormat.leaf(item.locator)))
        .accessibilityValue(Text(LocalizedStringKey(item.stage.titleKey)))
    }

    private var text: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(verbatim: ImportFormat.leaf(item.locator))
                .font(.subheadline)
                .lineLimit(1)
            ImportStatusLabel(titleKey: item.stage.titleKey, tone: item.stage.tone, symbol: item.stage.symbol)
                .font(.caption)
            failureText
        }
    }

    @ViewBuilder
    private var failureText: some View {
        if let code = item.failureCode {
            Text(LocalizedStringKey(code.rawValue))
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var trailing: some View {
        if isRetrying {
            ProgressView().controlSize(.small)
        } else if item.isRetryable {
            Button("app.import.run.retry", action: retry)
                .capsuleGlassButtonStyle()
                .controlSize(.small)
        }
    }
}

// MARK: - Previews

#Preview("Run — healthy") {
    NavigationStack {
        ImportExecutionView(plan: PreviewPlans.healthy, environment: .preview(.healthy))
    }
}

#Preview("Run — failures and retry") {
    NavigationStack {
        ImportExecutionView(plan: PreviewPlans.healthy, environment: .preview(.offline))
    }
}

#Preview("Run — nothing queued") {
    NavigationStack {
        ImportExecutionView(plan: PreviewPlans.empty, environment: .preview(.emptyLibrary))
    }
}

// MARK: - PreviewPlans

/// Fixed plans for the previews and the tests.
enum PreviewPlans {
    static let healthy = plan(itemCount: 40)
    static let empty = plan(itemCount: 0)

    static func plan(itemCount: Int, conflicts: [ImportConflict] = []) -> ImportPlan {
        ImportPlan(
            id: ImportID("preview-import"),
            scope: PreviewScopes.cameraRoll,
            destinationAlbumID: AlbumID.managed(uuid: "preview-album"),
            destinationRule: .scopeOverride,
            mode: .copy,
            uploadPolicy: .full,
            isStreaming: false,
            decisions: (0 ..< itemCount).map { ordinal in
                ImportDecision(
                    candidate: ImportCandidate(
                        id: "preview-\(ordinal)",
                        locator: "photokit://camera-roll/IMG_\(4000 + ordinal).HEIC",
                        contentType: .heic,
                        byteSize: 3400000
                    ),
                    action: .importAsset
                )
            },
            conflicts: conflicts
        )
    }
}
