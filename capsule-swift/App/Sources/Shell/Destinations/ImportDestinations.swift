import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import FeatureImport
import SwiftUI

extension RouteDestination {
    /// The Imports section: what has been imported, and the way to import more.
    var importsDestination: some View {
        ImportSectionView(environment: environment)
    }
}

// MARK: - ImportSectionView

/// The import history, with the pipeline that adds to it hanging off it.
///
/// The section's *route* is the history, because that is the part worth linking
/// to, restoring, and coming back to. The pipeline in front of it — pick a
/// source, scan it, confirm the plan, run it — is deliberately **not** routed:
/// a half-finished scan cannot be restored from a persisted route, and offering
/// a link that lands the user in the middle of a scan that no longer exists is
/// worse than not offering one. So the flow is a modal task with its position in
/// view state, and only its outcome is addressable.
struct ImportSectionView: View {
    let environment: AppEnvironment

    @State private var isImporting = false
    @State private var rerunPlan: ImportPlan?

    var body: some View {
        ImportHistoryView(
            environment: environment.importEnvironment,
            onRerun: { rerunPlan = $0 }
        )
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("ios.menu.import", systemImage: "square.and.arrow.down") {
                    isImporting = true
                }
                .accessibilityIdentifier("imports.start")
            }
        }
        .sheet(isPresented: $isImporting) {
            ImportFlowView(environment: environment) { isImporting = false }
        }
        .sheet(item: $rerunPlan) { plan in
            ImportFlowView(environment: environment, startingAt: .run(plan)) { rerunPlan = nil }
        }
    }
}

// MARK: - ImportFlowView

/// Pick a source, scan it, confirm what it would do, run it.
///
/// Each screen hands the next one the value it produced — a scope, then a scan,
/// then a plan — rather than re-deriving it, because re-scanning between the
/// confirmation and the run is exactly how a user ends up agreeing to one plan
/// and getting another.
struct ImportFlowView: View {
    let environment: AppEnvironment
    /// Called when the flow is done with itself, however it ended.
    let onFinish: @MainActor () -> Void

    @State private var step: Step

    /// Where the flow currently is. Not a `Route`: see ``ImportSectionView``.
    enum Step {
        case source
        case scan(ImportScope)
        case plan(ImportScan)
        case run(ImportPlan)
    }

    init(
        environment: AppEnvironment,
        startingAt step: Step = .source,
        onFinish: @escaping @MainActor () -> Void
    ) {
        self.environment = environment
        self.onFinish = onFinish
        _step = State(initialValue: step)
    }

    var body: some View {
        NavigationStack {
            content
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("ios.common.cancel") { onFinish() }
                    }
                }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch step {
        case .source:
            ImportSourcePickerView(environment: environment.importEnvironment) { scope in
                step = .scan(scope)
            }
        case let .scan(scope):
            ImportScanProgressView(
                scope: scope,
                environment: environment.importEnvironment,
                onScanned: { step = .plan($0) },
                onCancelled: { onFinish() }
            )
        case let .plan(scan):
            ImportPlanConfirmView(
                scan: scan,
                environment: environment.importEnvironment,
                onConfirm: { step = .run($0) }
            )
        case let .run(plan):
            ImportExecutionView(plan: plan, environment: environment.importEnvironment) { _ in
                onFinish()
            }
        }
    }
}
