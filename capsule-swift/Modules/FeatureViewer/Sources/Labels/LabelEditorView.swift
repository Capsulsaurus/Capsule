import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsuleUI
import ImagePipeline
import SwiftUI

// MARK: - LabelEditorView

/// The label and region editor, drawing ``LabelEditorModel``.
///
/// On an asset: its preview with the regions drawn over it (``RegionCanvas``),
/// the regions listed, the selected region's labels, and the stack's labels
/// when the asset is in one. On a stack or an album: its labels.
///
/// The shell is chosen by size class: a regular width puts the preview beside
/// the lists, a compact width stacks the lists under it. Every write goes
/// through the port and the editor redraws from its answer; a refusal is
/// shown under the lists and leaves the editor as it was.
public struct LabelEditorView: View {
    @State private var model: LabelEditorModel
    private let editing: LabelEditing
    private let asset: Asset?
    private let mediaLoader: ViewerMediaLoader?
    @State private var stackID: StackID?
    @State private var newLabel = ""
    @State private var firstLabel = ""
    @State private var isConfirmingDelete = false
    @Environment(\.horizontalSizeClass) private var sizeClass

    /// - Parameters:
    ///   - subject: what the editor is open on.
    ///   - editing: the port and the stack lookup.
    ///   - asset: the asset to draw regions over, for an asset subject.
    ///   - mediaLoader: where the preview's pixels come from.
    public init(
        subject: LabelEditorModel.Subject,
        editing: LabelEditing,
        asset: Asset? = nil,
        mediaLoader: ViewerMediaLoader? = nil
    ) {
        _model = State(wrappedValue: LabelEditorModel(subject: subject, port: editing.port))
        self.editing = editing
        self.asset = asset
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        content
            .navigationTitle(LocalizedStringKey(model.subject.titleKey))
            .task {
                await model.load()
                if case let .asset(assetID) = model.subject {
                    stackID = await editing.stackOf(assetID)
                }
            }
    }

    @ViewBuilder
    private var content: some View {
        if let asset, case .asset = model.subject {
            if sizeClass == .regular {
                HStack(spacing: 0) {
                    RegionCanvas(asset: asset, mediaLoader: mediaLoader, model: model)
                    form.frame(width: 360)
                }
            } else {
                VStack(spacing: 0) {
                    RegionCanvas(asset: asset, mediaLoader: mediaLoader, model: model)
                        .frame(height: 320)
                    form
                }
            }
        } else {
            form
        }
    }

    private var form: some View {
        Form {
            if case .asset = model.subject {
                drawingSection
                regionsSection
            }
            labelsSection
            stackSection
            failureSection
        }
        .disabled(model.isWriting)
    }

    // MARK: Drawing

    @ViewBuilder
    private var drawingSection: some View {
        Section {
            if model.draft == nil {
                Button {
                    model.startDrawing(.circle)
                } label: {
                    Label("app.labels.region.draw_circle", systemImage: "circle.dashed")
                }
                Button {
                    model.startDrawing(.rect)
                } label: {
                    Label("app.labels.region.draw_rect", systemImage: "rectangle.dashed")
                }
            } else {
                TextField("app.labels.region.first_label", text: $firstLabel)
                    .onSubmit(saveDraft)
                Button("app.labels.region.save", action: saveDraft)
                Button("app.common.cancel", role: .cancel) {
                    firstLabel = ""
                    model.cancelDrawing()
                }
            }
        } footer: {
            if model.draft != nil {
                Text("app.labels.draft.hint")
            }
        }
    }

    private func saveDraft() {
        let initial = firstLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? [] : [firstLabel]
        Task {
            await model.finishDrawing(labels: initial)
            if model.draft == nil { firstLabel = "" }
        }
    }

    // MARK: Regions

    private var regionsSection: some View {
        Section("app.labels.region.title") {
            if model.regions.isEmpty {
                Text("app.labels.region.empty")
                    .foregroundStyle(.secondary)
            }
            ForEach(model.regions) { region in
                Button {
                    model.selectRegion(region.id)
                } label: {
                    HStack {
                        Image(systemName: region.shape.isCircle ? "circle" : "rectangle")
                            .accessibilityHidden(true)
                        if let labels = region.displayLabels {
                            Text(verbatim: labels)
                        } else {
                            Text("app.labels.region.unlabelled")
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if region.id == model.selectedRegionID {
                            Image(systemName: "checkmark")
                                .accessibilityHidden(true)
                        }
                    }
                }
                .accessibilityAddTraits(region.id == model.selectedRegionID ? .isSelected : [])
            }
            if model.selectedRegionID != nil {
                Button("app.labels.region.delete", role: .destructive) {
                    isConfirmingDelete = true
                }
                .confirmationDialog(
                    "app.labels.region.delete.confirm",
                    isPresented: $isConfirmingDelete,
                    titleVisibility: .visible
                ) {
                    Button("app.common.delete", role: .destructive) {
                        Task { await model.deleteSelectedRegion() }
                    }
                }
            }
        }
    }

    // MARK: Labels

    @ViewBuilder
    private var labelsSection: some View {
        Section("app.labels.section") {
            if model.labelTarget == nil {
                Text("app.labels.region.none_selected")
                    .foregroundStyle(.secondary)
            } else {
                if model.labels.isEmpty {
                    Text("app.labels.empty")
                        .foregroundStyle(.secondary)
                }
                ForEach(model.labels, id: \.self) { label in
                    HStack {
                        Label {
                            Text(verbatim: label)
                        } icon: {
                            Image(systemName: "tag")
                        }
                        Spacer()
                        Button {
                            Task { await model.removeLabel(label) }
                        } label: {
                            Image(systemName: "minus.circle.fill")
                                .foregroundStyle(.red)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel("app.labels.remove")
                    }
                }
                HStack {
                    TextField("app.labels.add.placeholder", text: $newLabel)
                        .onSubmit(addLabel)
                    Button("app.labels.add", action: addLabel)
                        .disabled(newLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
    }

    private func addLabel() {
        let text = newLabel
        Task {
            await model.addLabel(text)
            if model.failure == nil { newLabel = "" }
        }
    }

    // MARK: Stack

    @ViewBuilder
    private var stackSection: some View {
        if let stackID {
            Section {
                NavigationLink {
                    LabelEditorView(subject: .stack(stackID), editing: editing)
                } label: {
                    Label("app.labels.stack.open", systemImage: "square.stack.3d.up")
                }
            }
        }
    }

    // MARK: Failure

    @ViewBuilder
    private var failureSection: some View {
        if let failure = model.failure {
            Section {
                Label(LocalizedStringKey(failure.messageKey), systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
        }
    }
}

// MARK: - LabelEditorSheet

/// The editor as a sheet: in its own navigation stack, with a Done button.
public struct LabelEditorSheet: View {
    private let subject: LabelEditorModel.Subject
    private let editing: LabelEditing
    private let asset: Asset?
    private let mediaLoader: ViewerMediaLoader?
    @Environment(\.dismiss) private var dismiss

    public init(
        subject: LabelEditorModel.Subject,
        editing: LabelEditing,
        asset: Asset? = nil,
        mediaLoader: ViewerMediaLoader? = nil
    ) {
        self.subject = subject
        self.editing = editing
        self.asset = asset
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            LabelEditorView(subject: subject, editing: editing, asset: asset, mediaLoader: mediaLoader)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("app.common.done") { dismiss() }
                    }
                }
        }
    }
}

// MARK: - LabelEditorButton

/// A button that opens the editor on a stack or an album, for a screen's
/// toolbar. Shows nothing where the environment carries no ``LabelEditing``.
public struct LabelEditorButton: View {
    private let subject: LabelEditorModel.Subject
    @Environment(\.labelEditing) private var editing
    @State private var isPresented = false

    public init(subject: LabelEditorModel.Subject) {
        self.subject = subject
    }

    public var body: some View {
        if let editing {
            Button {
                isPresented = true
            } label: {
                Label(LocalizedStringKey(subject.titleKey), systemImage: "tag")
            }
            .sheet(isPresented: $isPresented) {
                LabelEditorSheet(subject: subject, editing: editing)
            }
        }
    }
}
