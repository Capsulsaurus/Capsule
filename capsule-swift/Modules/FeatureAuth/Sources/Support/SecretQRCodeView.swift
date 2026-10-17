import CoreImage.CIFilterBuiltins
import SwiftUI

// MARK: - SecretQRCodeView

/// Renders a secret payload as a QR code.
///
/// Used for the cross-device enrollment payload and the TOTP provisioning URI.
/// Both are secrets, and the rule for both is the same: they are **drawn and
/// nothing else**. The payload never reaches a log, a file, a cache, or an
/// analytics event, and this view keeps no copy of it beyond the render.
///
/// Built on Core Image rather than a platform view so it works identically on
/// iPhone, iPad, and Mac without a `Platform/` shim: `Image(decorative:scale:)`
/// takes a `CGImage`, which is the one image currency both platforms share.
/// `decorative` because the code is not describable — the accessibility label
/// belongs on the surrounding text, which offers the transcribable fallback.
struct SecretQRCodeView: View {
    let payload: String
    var side: CGFloat = 220

    var body: some View {
        Group {
            if let image = Self.render(payload, side: side) {
                Image(decorative: image, scale: 1)
                    .interpolation(.none)
                    .resizable()
                    .aspectRatio(1, contentMode: .fit)
            } else {
                // A QR code that cannot be rendered is not an error the user can
                // act on — the text fallback beside it is the answer, so the
                // placeholder stays quiet rather than raising an alarm.
                RoundedRectangle(cornerRadius: 8)
                    .fill(.quaternary)
                    .aspectRatio(1, contentMode: .fit)
                    .overlay(Image(systemName: "qrcode").font(.largeTitle).foregroundStyle(.secondary))
            }
        }
        .frame(width: side, height: side)
        .accessibilityHidden(true)
    }

    /// Render the payload, or `nil` if Core Image declines.
    ///
    /// `M` correction, which tolerates a scuffed screen or a poor camera without
    /// inflating the module count to the point where a phone camera cannot
    /// resolve it at this size.
    private static func render(_ payload: String, side: CGFloat) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scale = max(1, side / max(output.extent.width, 1))
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        return CIContext().createCGImage(scaled, from: scaled.extent)
    }
}
