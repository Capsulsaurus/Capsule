import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ExportSheet

/// What leaves the library when photographs are exported
/// (*Metadata — Privacy on Export*).
///
/// The list is shown in full before anything happens, with the per-export
/// retain switch beside it. The switch is off every time the sheet opens; there
/// is no account setting behind it, on purpose.
public struct ExportSheet: View {
    @State private var model: ExportViewModel
    @Environment(\.dismiss) private var dismiss

    public init(model: ExportViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        NavigationStack {
            Form {
                summarySection
                stripSection
                if let outcome = model.outcome {
                    outcomeSection(outcome)
                }
            }
            .formStyle(.grouped)
            .frame(maxWidth: 640)
            .frame(maxWidth: .infinity)
            .navigationTitle("app.export.title")
            .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("app.common.cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("app.export.action") {
                        Task { await model.export() }
                    }
                    .disabled(model.isExporting || model.assetIDs.isEmpty)
                }
            }
        }
        .task { await model.prepare() }
    }

    // MARK: Sections

    private var summarySection: some View {
        Section {
            LabeledContent {
                Text(model.assetIDs.count, format: .number)
            } label: {
                Text("app.export.summary.count")
            }
            if model.isExporting {
                ProgressView()
                    .accessibilityLabel("app.export.progress")
            }
        } footer: {
            Text("app.export.summary.footer")
        }
    }

    /// The strip itself, plus the opt-in. The rows re-render as the switch moves
    /// so the consequence of flipping it is visible, not implied.
    private var stripSection: some View {
        Section {
            PrivacyStripView(policy: model.policy, setRetention: model.setRetention)
        } header: {
            Text("app.export.strip.header")
        } footer: {
            Text("app.export.strip.footer")
        }
    }

    @ViewBuilder
    private func outcomeSection(_ outcome: ExportViewModel.Outcome) -> some View {
        Section {
            switch outcome {
            case .prepared:
                Label("app.export.outcome.prepared", systemImage: "checkmark.circle")
                    .accessibilityElement(children: .combine)
            case let .partial(_, unavailable):
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                    Label("app.export.outcome.partial", systemImage: "exclamationmark.circle")
                    LabeledContent {
                        Text(unavailable, format: .number)
                    } label: {
                        Text("app.export.outcome.unavailable_count")
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
        } footer: {
            // Says out loud that the opt-in has already gone back off.
            Text("app.export.outcome.footer")
        }
    }
}

// MARK: - Previews

#Preview("Export — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return ExportSheet(model: ExportViewModel(
        assetIDs: [.managed(uuid: "preview-a"), .managed(uuid: "preview-b")],
        sync: environment.sync,
        connectivity: SharingConnectivity(sync: environment.sync)
    ))
    .preferredColorScheme(.light)
}

#Preview("Export — dark, offline") {
    let environment = MockEnvironment(scenario: .offline)
    return ExportSheet(model: ExportViewModel(
        assetIDs: [.managed(uuid: "preview-a")],
        sync: environment.sync,
        connectivity: SharingConnectivity(sync: environment.sync)
    ))
    .preferredColorScheme(.dark)
}
