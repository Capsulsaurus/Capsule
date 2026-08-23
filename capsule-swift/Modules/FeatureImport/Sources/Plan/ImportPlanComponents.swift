import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - ImportSummaryCard

/// "1 482 items · 24.3 GB" — the one line that says how big this is.
///
/// A card rather than a row because it is the screen's headline: everything
/// below it qualifies this number, and a user who reads nothing else must still
/// leave knowing what they agreed to.
struct ImportSummaryCard: View {
    let itemCount: Int
    let byteCount: UInt64

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(verbatim: headline)
                .font(.title2.weight(.semibold))
                .monospacedDigit()
            Text("app.import.plan.summary.caption")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, CapsuleTheme.Spacing.small)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text("app.import.plan.summary.caption"))
        .accessibilityValue(Text(verbatim: headline))
    }

    /// Composed from a translatable separator template and two locale-formatted
    /// values, never from a hardcoded middle dot: the separator is punctuation,
    /// and punctuation is a translator's business.
    private var headline: String {
        String(
            format: String(localized: "app.import.plan.summary.headline"),
            ImportFormat.count(itemCount),
            ImportFormat.bytes(byteCount)
        )
    }
}

// MARK: - ImportStatTile

/// One of the three counts, tappable to open its list.
///
/// The count is the affordance: it is the largest thing on the tile, the label
/// sits under it, and the whole tile is one accessibility element so VoiceOver
/// reads "Conflicts, 4, button" rather than two disconnected fragments.
struct ImportStatTile: View {
    let category: ImportPlanCategory
    let count: Int
    let isExpanded: Bool
    let tone: ImportTone
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            tileBody
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(LocalizedStringKey(category.titleKey)))
        .accessibilityValue(Text(verbatim: ImportFormat.count(count)))
        .accessibilityAddTraits(isExpanded ? [.isButton, .isSelected] : .isButton)
    }

    private var tileBody: some View {
        VStack(spacing: CapsuleTheme.Spacing.xxSmall) {
            Image(systemName: category.symbol)
                .foregroundStyle(tone.tint)
                .imageScale(.small)
            Text(verbatim: ImportFormat.count(count))
                .font(.title3.weight(.semibold))
                .monospacedDigit()
            Text(LocalizedStringKey(category.titleKey))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, CapsuleTheme.Spacing.small)
        .background(background)
        .contentShape(Rectangle())
    }

    private var background: some View {
        RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card, style: .continuous)
            .fill(Color.secondary.opacity(isExpanded ? 0.18 : 0.08))
    }
}

// MARK: - ImportSpaceMeter

/// Free space, measured against this plan.
///
/// Three states, and the severe one names its remedy. Colour is reinforcement
/// only: each state pairs its tint with its own symbol and its own sentence, so
/// the meter survives greyscale and reads correctly to VoiceOver.
struct ImportSpaceMeter: View {
    let outlook: ImportSpaceOutlook
    let itemCount: Int
    let onFreeUpSpace: (@MainActor () -> Void)?
    let onEnableStreaming: (@MainActor () -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            bar
            figures
            verdict
            action
        }
        .padding(.vertical, CapsuleTheme.Spacing.xSmall)
    }

    private var bar: some View {
        ProgressView(value: outlook.fractionOfAvailable)
            .tint(outlook.state.tone.tint)
            .accessibilityLabel(Text("app.import.plan.space.header"))
            .accessibilityValue(Text(verbatim: ImportFormat.percent(outlook.fractionOfAvailable)))
    }

    private var figures: some View {
        VStack(spacing: CapsuleTheme.Spacing.xxSmall) {
            ImportValueRow(
                labelKey: "app.import.plan.space.required",
                value: ImportFormat.bytes(outlook.requiredBytes)
            )
            ImportValueRow(
                labelKey: "app.import.plan.space.available",
                value: ImportFormat.bytes(outlook.availableBytes)
            )
        }
    }

    private var verdict: some View {
        Label {
            Text(verbatim: sentence)
                .font(.footnote)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: outlook.state.tone.symbol)
        }
        .foregroundStyle(outlook.state.tone.tint)
    }

    /// The severe case states the count and the exact shortfall, because "not
    /// enough space" is not actionable and "free 4.1 GB" is.
    private var sentence: String {
        switch outlook.state {
        case .comfortable:
            String(localized: "app.import.plan.space.comfortable")
        case .streamingRecommended:
            String(localized: "app.import.plan.space.streaming")
        case .insufficient:
            String(
                format: String(localized: "app.import.plan.space.insufficient"),
                ImportFormat.count(itemCount),
                ImportFormat.bytes(outlook.shortfallBytes)
            )
        }
    }

    @ViewBuilder
    private var action: some View {
        switch outlook.state {
        case .comfortable:
            EmptyView()
        case .streamingRecommended:
            Button("app.import.plan.space.enable_streaming") { onEnableStreaming?() }
                .buttonStyle(.bordered)
                .disabled(onEnableStreaming == nil)
        case .insufficient:
            Button("app.import.plan.space.free_up") { onFreeUpSpace?() }
                .buttonStyle(.borderedProminent)
                .disabled(onFreeUpSpace == nil)
        }
    }
}

// MARK: - ImportDestinationRow

/// "Goes to **Camera Roll**", and underneath it, why.
///
/// The reason line is not decoration. Resolution is five rungs deep and a scope
/// override fires without the user doing anything, so a destination shown alone
/// is a destination nobody can account for later. There is deliberately no code
/// path here that renders one without the other.
struct ImportDestinationRow: View {
    let albumName: String?
    let rule: ImportPlan.DestinationRule

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            destination
            reason
        }
        .accessibilityElement(children: .combine)
    }

    private var destination: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.xSmall) {
            Text("app.import.plan.destination.title")
                .foregroundStyle(.secondary)
            albumLabel
                .fontWeight(.semibold)
        }
    }

    @ViewBuilder
    private var albumLabel: some View {
        if let albumName, !albumName.isEmpty {
            Text(verbatim: albumName)
        } else {
            Text("app.import.plan.destination.unnamed")
        }
    }

    private var reason: some View {
        Text(String(format: String(localized: "app.import.plan.destination.reason"), String(localized: String.LocalizationValue(rule.reasonKey))))
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: - ImportDecisionRow

/// One candidate in an expanded Add or Skip list.
struct ImportDecisionRow: View {
    let decision: ImportDecision

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(verbatim: ImportFormat.leaf(decision.candidate.locator))
                    .font(.subheadline)
                reason
            }
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Text(verbatim: ImportFormat.bytes(decision.candidate.byteSize))
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var reason: some View {
        if let key = decision.action.skipReasonKey {
            Text(LocalizedStringKey(key))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - ImportConflictRow

/// One conflict, with the choice attached to it.
///
/// The picker offers only the resolutions the conflict's kind admits, and the
/// destructive one carries a warning next to it rather than being styled
/// identically to the others.
struct ImportConflictRow: View {
    let conflict: ImportConflict
    let resolve: (ImportConflictResolution) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(verbatim: ImportFormat.leaf(conflict.locator))
                .font(.subheadline)
            Text(LocalizedStringKey(conflict.kind.titleKey))
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            picker
            destructiveNote
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
    }

    private var picker: some View {
        Picker(selection: selection) {
            ForEach(conflict.kind.allowedResolutions, id: \.rawValue) { resolution in
                Text(LocalizedStringKey(resolution.titleKey)).tag(resolution)
            }
        } label: {
            Text("app.import.plan.conflict.resolution.header")
        }
        .pickerStyle(.menu)
        .accessibilityLabel(Text("app.import.plan.conflict.resolution.header"))
    }

    private var selection: Binding<ImportConflictResolution> {
        Binding(get: { conflict.resolution }, set: resolve)
    }

    @ViewBuilder
    private var destructiveNote: some View {
        if conflict.resolution.isDestructive {
            ImportStatusLabel(titleKey: "app.import.conflict.destructive.note", tone: .critical)
                .font(.caption)
        }
    }
}
