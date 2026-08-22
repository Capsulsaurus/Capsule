import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - QuarantineInboxView

/// Triage for everything held for a human decision, grouped by the eight
/// quarantine surfaces of the threat model's own table.
///
/// The empty state is the **good** state, and says so.
///
/// Route entry point. Ports required: ``QuarantinePort``, ``LibraryPort``
/// (thumbnails for the asset-scoped rows), ``SyncPort`` (connection class).
public struct QuarantineInboxView: View {
    @State private var model: QuarantineInboxModel
    @State private var inspected: QuarantineItem?
    private let quarantine: any QuarantinePort
    private let sync: any SyncPort

    public init(quarantine: any QuarantinePort, library: any LibraryPort, sync: any SyncPort) {
        _model = State(wrappedValue: QuarantineInboxModel(
            quarantine: quarantine,
            library: library,
            sync: sync
        ))
        self.quarantine = quarantine
        self.sync = sync
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("ios.quarantine.title")
        }
        .task { await model.load() }
        .inspector(isPresented: .constant(inspected != nil)) { inspector }
    }

    @ViewBuilder
    private var inspector: some View {
        if let inspected {
            QuarantineDetailView(item: inspected, quarantine: quarantine, sync: sync) {
                self.inspected = nil
            }
            .inspectorColumnWidth(min: 320, ideal: 400, max: 560)
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                ForEach(model.groups) { group in
                    Section {
                        ForEach(group.items) { item in
                            Button { inspected = item } label: {
                                QuarantineRowView(item: item, asset: model.asset(for: item))
                            }
                            .buttonStyle(.plain)
                        }
                    } header: {
                        Label(LocalizedStringKey(group.surface.badge.titleKey), systemImage: group.surface.badge.systemImage)
                    } footer: {
                        Text(group.surface.explanationKey)
                    }
                }
            }
            .listStyle(.inset)
            .refreshable { await model.reload() }
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "ios.quarantine.empty.title",
                emptyDescription: "ios.quarantine.empty.description",
                emptySymbol: "checkmark.shield",
                retry: { await model.reload() }
            )
        }
    }
}

// MARK: - QuarantineRowView

/// One held item: what it is, why, and when.
///
/// The thumbnail degrades to a glyph rather than to a blank: an item quarantined
/// *because* it would not decode has no LQIP to draw, and a grey rectangle
/// there would look like a loading state that never finishes.
struct QuarantineRowView: View {
    let item: QuarantineItem
    let asset: LibraryAsset?

    var body: some View {
        HStack(alignment: .top, spacing: CapsuleTheme.Spacing.medium) {
            thumbnail
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(LocalizedStringKey(item.surface.badge.titleKey))
                    .font(.body)
                Text(verbatim: item.reason.code)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Text(verbatim: TransferFormat.captureDate(item.detectedAt))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if item.surface.storage.preservesOriginalBytes {
                    Label("ios.quarantine.row.preserved", systemImage: "archivebox")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var thumbnail: some View {
        if let asset, asset.lqip != nil {
            AssetSwatch(colour: asset.lqip?.dominantColor, mediaType: asset.mediaType)
        } else {
            RoundedRectangle(cornerRadius: CapsuleTheme.Radius.small, style: .continuous)
                .fill(Color.secondary.opacity(0.15))
                .frame(width: 44, height: 44)
                .overlay(
                    Image(systemName: item.surface.badge.systemImage)
                        .foregroundStyle(item.surface.badge.tint)
                )
                .accessibilityHidden(true)
        }
    }
}

// MARK: - Previews

#Preview("Six surfaces populated") {
    let environment = MockEnvironment(scenario: .quarantine)
    return QuarantineInboxView(
        quarantine: environment.quarantine,
        library: environment.library,
        sync: environment.sync
    )
}

#Preview("Nothing held — the good state") {
    let environment = MockEnvironment(scenario: .healthy)
    return QuarantineInboxView(
        quarantine: environment.quarantine,
        library: environment.library,
        sync: environment.sync
    )
    .preferredColorScheme(.dark)
}
