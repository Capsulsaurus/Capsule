import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - ImportAndScopesSettingsView

/// Import — the default album, the scope table, and the resolution order.
public struct ImportAndScopesSettingsView: View {
    @State private var model: ImportAndScopesSettingsModel

    public init(model: ImportAndScopesSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: ImportAndScopesSettingsModel(
                settings: environment.settings,
                importing: environment.importing,
                albums: environment.albums,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.importAndScopes.titleKey,
            phase: model.phase,
            emptyTitleKey: "ios.settings.import.empty.title",
            emptyDescriptionKey: "ios.settings.import.empty.description",
            retry: { await model.load() },
            content: {
                defaultAlbumSection
                resolutionOrderSection
                scopeSection
                sourceKindSection
            }
        )
        .task { await model.load() }
    }

    // MARK: Default album

    private var defaultAlbumSection: some View {
        Section {
            Picker(
                "ios.settings.import.default.label",
                selection: ownerDefaultBinding
            ) {
                ForEach(model.albums) { album in
                    albumLabel(album.id).tag(AlbumID?.some(album.id))
                }
            }
            .pickerStyle(.menu)
        } header: {
            Text("ios.settings.import.default.header")
        } footer: {
            Text("ios.settings.import.default.footer")
        }
    }

    private var ownerDefaultBinding: Binding<AlbumID?> {
        Binding(
            get: { model.ownerDefaultAlbumID ?? model.derivedDefaultAlbumID },
            set: { newValue in
                guard let newValue else { return }
                Task { await model.setOwnerDefault(newValue) }
            }
        )
    }

    // MARK: Resolution order

    /// The five rungs, in order, numbered.
    ///
    /// Drawn even where a rung cannot fire in this build, because the ladder is
    /// the explanation — a user who cannot see that an explicit pick outranks
    /// their override will keep re-setting the override.
    private var resolutionOrderSection: some View {
        Section {
            ForEach(Array(DestinationResolution.order.enumerated()), id: \.offset) { index, rule in
                SettingsValueRow(
                    labelKey: DestinationResolution.titleKey(for: rule),
                    value: SettingsFormat.count(index + 1)
                )
            }
        } header: {
            Text("ios.settings.import.order.header")
        } footer: {
            Text("ios.settings.import.order.footer")
        }
    }

    // MARK: Sources

    private var scopeSection: some View {
        Section {
            if model.resolutions.isEmpty {
                SettingsNoteRow(textKey: "ios.settings.import.sources.none")
            }
            ForEach(model.resolutions) { row in
                scopeRows(row)
            }
        } header: {
            Text("ios.settings.import.sources.header")
        } footer: {
            Text("ios.settings.import.sources.footer")
        }
    }

    @ViewBuilder
    private func scopeRows(_ row: ScopeResolutionRow) -> some View {
        SettingsValueRow(
            labelKey: DestinationResolution.titleKey(for: row.scope.sourceKind),
            value: SettingsFormat.shortIdentifier(row.scope.locator, length: 28)
        )
        SettingsStatusRow(
            labelKey: "ios.settings.import.sources.rule",
            statusKey: DestinationResolution.titleKey(for: row.rule),
            tone: row.rule == .scopeOverride ? .positive : .neutral
        )
        Picker(
            "ios.settings.import.sources.destination",
            selection: overrideBinding(for: row.scope)
        ) {
            Text("ios.settings.import.sources.unset").tag(AlbumID?.none)
            ForEach(model.albums) { album in
                albumLabel(album.id).tag(AlbumID?.some(album.id))
            }
        }
        .pickerStyle(.menu)
    }

    private func overrideBinding(for scope: ImportScope) -> Binding<AlbumID?> {
        Binding(
            get: { model.overrides[scope] },
            set: { newValue in Task { await model.setOverride(newValue, for: scope) } }
        )
    }

    // MARK: Per-source-kind defaults

    private var sourceKindSection: some View {
        Section {
            ForEach(SourceKind.knownCases, id: \.rawValue) { kind in
                SettingsValueRow(
                    labelKey: DestinationResolution.titleKey(for: kind),
                    value: sourceKindValue(kind)
                )
            }
        } header: {
            Text("ios.settings.import.kinds.header")
        } footer: {
            Text("ios.settings.import.kinds.footer")
        }
    }

    private func sourceKindValue(_ kind: SourceKind) -> String {
        guard let albumID = model.sourceKindDefaults[kind] else {
            return SettingsPhrase.text(forKey: "ios.settings.import.sources.unset")
        }
        return model.albumName(albumID)
            ?? SettingsPhrase.text(forKey: "ios.settings.import.album.unnamed_default")
    }

    // MARK: Shared

    @ViewBuilder
    private func albumLabel(_ albumID: AlbumID?) -> some View {
        if let albumID, let name = model.albumName(albumID) {
            Text(verbatim: name)
        } else {
            Text("ios.settings.import.album.unnamed_default")
        }
    }
}

// MARK: - Preview

#Preview("Import & Scopes") {
    NavigationStack {
        ImportAndScopesSettingsView(environment: .preview())
    }
}

#Preview("Import & Scopes — Empty") {
    NavigationStack {
        ImportAndScopesSettingsView(environment: .preview(.emptyLibrary))
    }
}
