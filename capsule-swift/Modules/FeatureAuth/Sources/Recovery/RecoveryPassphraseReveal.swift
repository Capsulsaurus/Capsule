import CapsuleUI
import SwiftUI

// MARK: - RecoveryWordGrid

/// The passphrase itself, as a numbered monospaced grid inside a bordered card.
///
/// Three details are load-bearing rather than decorative:
///
/// - **Numbered.** The type-back gate asks for "word 7", so the words have to be
///   countable on the page the user is copying from. A wrapped paragraph is not.
/// - **Monospaced.** The words go onto paper or into a password manager, and
///   `rn`/`m` or `l`/`1` confusion in a recovery secret is unrecoverable.
/// - **Bordered.** The card marks where the secret starts and stops, so a user
///   photographing or transcribing it does not clip the last row.
struct RecoveryWordGrid: View {
    let words: [String]

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: 130), spacing: CapsuleTheme.Spacing.small, alignment: .leading)]
    }

    var body: some View {
        LazyVGrid(columns: columns, alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            ForEach(Array(words.enumerated()), id: \.offset) { offset, word in
                cell(position: offset + 1, word: word)
            }
        }
        .padding(CapsuleTheme.Spacing.large)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large))
        .overlay(
            RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large)
                .strokeBorder(.separator, lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel("app.recovery.passphrase.grid.header")
    }

    private func cell(position: Int, word: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Text(verbatim: "\(position)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(minWidth: 20, alignment: .trailing)
            Text(verbatim: word)
                .font(.body.monospaced().weight(.medium))
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("app.recovery.passphrase.word_position")
        .accessibilityValue(Text(verbatim: "\(position), \(word)"))
    }
}

// MARK: - RecoveryEntropyMeter

/// The strength meter, reporting a **threshold verdict** rather than a vibe.
///
/// *Backup & Recovery — Master-Key Escrow* puts the security in the secret
/// itself: the escrow blob is offline-attackable once exfiltrated and Argon2id
/// raises brute-force cost only linearly, so the phrase must carry ≥128 bits.
/// That is a line, not a gradient, and a five-bar gauge reading "strong" at 66
/// bits would be lying about the only number that matters — so the bar is drawn
/// *against the floor* and the number beside it is stated outright.
///
/// A secret below the floor is a **defect in this build**, not a user error: the
/// user picked nothing, the generator did. The copy says so instead of nagging
/// them to choose something stronger.
struct RecoveryEntropyMeter: View {
    let estimate: RecoveryEntropyEstimate

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("app.recovery.passphrase.entropy.label")
                .font(.headline)
            ProgressView(value: estimate.fraction)
                .tint(estimate.meetsFloor ? .green : .red)
                .accessibilityLabel("app.recovery.passphrase.entropy.label")
                .accessibilityValue(Text(verbatim: measurement))
            Text(verbatim: measurement)
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
            StatusChip(
                titleKey: estimate.meetsFloor
                    ? "app.recovery.passphrase.entropy.meets_floor"
                    : "app.recovery.passphrase.entropy.below_floor",
                symbolName: estimate.meetsFloor ? "checkmark.seal.fill" : "exclamationmark.octagon.fill",
                tint: estimate.meetsFloor ? .green : .red
            )
            Text(
                estimate.meetsFloor
                    ? "app.recovery.passphrase.entropy.footnote"
                    : "app.recovery.passphrase.entropy.defect"
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .authCard()
    }

    /// "66 / 128 bits" as data, never as a catalog key: a key with a number
    /// interpolated into it is a key no catalog contains.
    private var measurement: String {
        "\(estimate.bits) / \(estimate.floorBits)"
    }
}

// MARK: - Previews

#Preview("Recovery word grid") {
    ScrollView {
        VStack(spacing: CapsuleTheme.Spacing.large) {
            RecoveryWordGrid(words: [
                "harbor", "lantern", "quartz", "meadow",
                "cobalt", "thistle", "ember", "willow",
                "granite", "cinder", "marlin", "juniper",
            ])
            RecoveryEntropyMeter(estimate: RecoveryEntropy.estimate(wordCount: 12))
            RecoveryEntropyMeter(estimate: RecoveryEntropy.estimate(wordCount: 6))
        }
        .padding()
    }
}
