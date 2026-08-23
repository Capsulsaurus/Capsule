import CapsuleDomain
import CapsuleMock
import SwiftUI
import UniformTypeIdentifiers

// MARK: - ImportSourcePickerView

/// **Step 1 — where the photos are coming from.**
///
/// Every row says what it will do, because the five sources differ in ways a
/// name does not convey: one reads a library the app already has permission to,
/// one asks the OS for a folder, one keeps watching that folder afterwards, and
/// one unpacks an archive written by somebody else's product.
///
/// macOS-only sources are **absent** on a handheld rather than disabled. A
/// greyed-out row implies a permission the user could grant, and there is none
/// to grant.
public struct ImportSourcePickerView: View {
    @State private var model: ImportSourcePickerModel
    @State private var pendingPick: ImportSourceRow?
    private let onScopeChosen: (@MainActor (ImportScope) -> Void)?

    public init(
        model: ImportSourcePickerModel,
        onScopeChosen: (@MainActor (ImportScope) -> Void)? = nil
    ) {
        _model = State(initialValue: model)
        self.onScopeChosen = onScopeChosen
    }

    public init(
        environment: ImportEnvironment,
        onScopeChosen: (@MainActor (ImportScope) -> Void)? = nil
    ) {
        self.init(model: ImportSourcePickerModel(environment: environment), onScopeChosen: onScopeChosen)
    }

    public var body: some View {
        ImportScreen(
            titleKey: "app.import.source.title",
            phase: model.phase,
            emptyTitleKey: "app.import.source.empty.title",
            emptyDescriptionKey: "app.import.source.empty.description",
            emptySymbol: "square.and.arrow.down",
            retry: { await model.load() },
            content: { sourceList }
        )
        .task { await model.load() }
        .fileImporter(
            isPresented: pickerPresented,
            allowedContentTypes: allowedTypes,
            allowsMultipleSelection: false,
            onCompletion: acceptPick
        )
    }

    private var sourceList: some View {
        List {
            Section {
                ForEach(model.rows) { row in
                    sourceButton(row)
                }
            } header: {
                Text("app.import.source.header")
            } footer: {
                Text("app.import.source.footer")
            }
        }
    }

    private func sourceButton(_ row: ImportSourceRow) -> some View {
        Button {
            activate(row)
        } label: {
            ImportSourceRowLabel(row: row)
        }
        .buttonStyle(.plain)
        .accessibilityHint(Text(LocalizedStringKey(row.option.detailKey)))
    }

    // MARK: Behaviour

    private func activate(_ row: ImportSourceRow) {
        if let scope = model.select(row) {
            onScopeChosen?(scope)
            return
        }
        pendingPick = row
    }

    private var pickerPresented: Binding<Bool> {
        Binding(
            get: { pendingPick != nil },
            set: { presented in if !presented { pendingPick = nil } }
        )
    }

    /// A Takeout archive is a file; every other pickable source is a directory.
    private var allowedTypes: [UTType] {
        pendingPick?.option.kind == .takeoutArchive ? [.zip, .archive] : [.folder]
    }

    private func acceptPick(_ result: Result<[URL], any Error>) {
        guard let row = pendingPick, case let .success(urls) = result, let url = urls.first else {
            pendingPick = nil
            return
        }
        pendingPick = nil
        Task {
            if let scope = await model.choose(row, locator: url.absoluteString) {
                onScopeChosen?(scope)
            }
        }
    }
}

// MARK: - ImportSourceRowLabel

/// One source row: what it is, what it will do, and whether it is ready.
private struct ImportSourceRowLabel: View {
    let row: ImportSourceRow

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: row.option.symbol)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            text
            Spacer(minLength: 8)
            Image(systemName: "chevron.forward")
                .font(.footnote)
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var text: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(LocalizedStringKey(row.option.titleKey))
                .font(.body)
            Text(LocalizedStringKey(row.option.detailKey))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            status
        }
    }

    @ViewBuilder
    private var status: some View {
        if row.scansImmediately {
            ImportStatusLabel(titleKey: "app.import.source.ready", tone: .positive)
                .font(.caption)
        } else {
            ImportStatusLabel(titleKey: "app.import.source.choose", tone: .neutral, symbol: "hand.tap")
                .font(.caption)
        }
    }
}

// MARK: - Previews

#Preview("Sources — Mac") {
    NavigationStack {
        ImportSourcePickerView(environment: .preview(.healthy, platform: .desktop))
    }
}

#Preview("Sources — handheld") {
    NavigationStack {
        ImportSourcePickerView(environment: .preview(.healthy, platform: .handheld))
    }
}

#Preview("Sources — offline") {
    NavigationStack {
        ImportSourcePickerView(environment: .preview(.offline, platform: .handheld))
    }
}

#Preview("Sources — empty library") {
    NavigationStack {
        ImportSourcePickerView(environment: .preview(.emptyLibrary, platform: .handheld))
    }
}
