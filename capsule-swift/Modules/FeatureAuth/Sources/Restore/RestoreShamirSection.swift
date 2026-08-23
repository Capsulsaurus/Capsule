import CapsuleUI
import SwiftUI

// MARK: - RestoreShamirSection

/// Share entry for the 2-of-3 split.
///
/// Any two shares reconstruct the seed; one alone reveals nothing, and
/// reconstruction is entirely client-side — the server never holds more than one
/// share at a time. The screen therefore asks for a *quorum*, never for "all
/// your shares", and says how many are still needed rather than leaving the user
/// to work out why the button is disabled.
///
/// An invalidated share is **shown, not hidden**. A rotation kills old shares,
/// and a user holding dead material has to learn it is dead *now* — not on the
/// day they need it. It is listed, marked, and refused as part of the quorum.
struct RestoreShamirSection: View {
    let shares: [ShamirShareSummary]
    let selected: Set<String>
    let threshold: Int
    let canReconstruct: Bool
    let isWorking: Bool
    let toggle: (String) -> Void
    let reconstruct: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "ios.restore.shamir.title",
                descriptionKey: "ios.restore.shamir.description",
                symbolName: "person.3.sequence.fill"
            )
            thresholdLine
            if shares.isEmpty {
                Text("ios.restore.shamir.empty")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(shares) { share in
                    shareRow(share)
                }
            }
            actions
        }
        .authCard()
    }

    private var thresholdLine: some View {
        HStack(spacing: CapsuleTheme.Spacing.xSmall) {
            Text("ios.restore.shamir.threshold")
                .font(.callout)
                .foregroundStyle(.secondary)
            Text(verbatim: "\(threshold)")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private func shareRow(_ share: ShamirShareSummary) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            // The label is user-authored — "safe deposit box", "my sister" — so
            // it is shown verbatim and never routed through the catalog.
            Toggle(isOn: selectionBinding(for: share)) {
                Text(verbatim: share.label)
                    .font(.headline)
            }
            .disabled(share.isInvalidated)
            .accessibilityLabel(Text(verbatim: share.label))

            AuthLabeledDate(labelKey: "ios.restore.shamir.issued", date: share.issuedAt.date)
            if share.isInvalidated {
                StatusChip(
                    titleKey: "ios.restore.shamir.invalidated",
                    symbolName: "xmark.seal.fill",
                    tint: .orange
                )
                Text("ios.restore.shamir.invalidated_note")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .authInnerCard()
    }

    private var actions: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            if isWorking {
                AuthLoadingView(labelKey: "ios.restore.shamir.reconstructing")
            }
            Button("ios.restore.shamir.reconstruct", action: reconstruct)
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(!canReconstruct || isWorking)
                .accessibilityLabel("ios.restore.shamir.reconstruct")
        }
    }

    private func selectionBinding(for share: ShamirShareSummary) -> Binding<Bool> {
        Binding(
            get: { selected.contains(share.id) },
            set: { _ in toggle(share.id) }
        )
    }
}
