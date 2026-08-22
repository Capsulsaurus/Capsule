import CapsuleUI
import SwiftUI

// MARK: - ServerIdentityCard

/// What `.well-known/capsule/server-info` said, laid out so the one field that
/// matters — the signing key — is the one a user can actually check.
///
/// The key is rendered in a monospaced font, chunked by
/// ``ChunkedCodeFormatter``, and left selectable: a user comparing it against
/// what their administrator published needs to be able to copy it, and a
/// proportional font makes `0`/`O` and `1`/`l` indistinguishable in exactly the
/// place that cannot afford it.
///
/// Nothing here is a user list. The document is public and server-scoped
/// precisely because a `.well-known` that enumerated users would be an abuse and
/// privacy surface, so there is nothing on this card that names a person.
struct ServerIdentityCard: View {
    let server: ServerInfo
    let signingKeyDisplay: String
    let isCompatible: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            Text("ios.auth.server.identity.header")
                .font(.headline)
            field(labelKey: "ios.auth.server.identity.origin", value: server.origin)
            field(labelKey: "ios.auth.server.identity.api", value: server.apiBaseURL.absoluteString)
            signingKey
            protocolRow
            Text("ios.auth.server.identity.footer")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.large)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large))
    }

    private var signingKey: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text("ios.auth.server.identity.signing_key")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: signingKeyDisplay)
                .font(.body.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityLabel("ios.auth.server.identity.signing_key")
                .accessibilityValue(Text(verbatim: signingKeyDisplay))
        }
    }

    @ViewBuilder
    private var protocolRow: some View {
        let range = server.supportedProtocolVersions
        field(
            labelKey: "ios.auth.server.protocol.supported",
            value: "\(range.lowerBound)–\(range.upperBound)"
        )
        if !isCompatible {
            Label {
                Text("ios.auth.server.protocol.incompatible")
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "exclamationmark.triangle.fill")
            }
            .font(.footnote)
            .foregroundStyle(.orange)
        }
    }

    private func field(labelKey: LocalizedStringKey, value: String) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(labelKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: value)
                .font(.callout)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}
