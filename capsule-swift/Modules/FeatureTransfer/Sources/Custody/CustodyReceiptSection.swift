import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - CustodyReceiptSection

/// One receipt: what was attested, when, and by which key.
///
/// Every field on screen answers one of those three questions, because that is
/// what makes the receipt *evidence* rather than a status line. The signature
/// itself is opaque here — verification belongs to `capsule-core` — so the
/// screen reports the key that signed and the log position rather than
/// pretending to have checked the bytes.
///
/// Owning doc: *Storage Verification — Custody Receipts*.
struct CustodyReceiptSection: View {
    let receipt: CustodyReceipt

    var body: some View {
        Section {
            LabeledContent("ios.custody.field.blob_role") {
                Text(verbatim: receipt.blobRole.rawValue)
            }
            LabeledContent("ios.custody.field.size") {
                Text(verbatim: TransferFormat.bytes(receipt.size))
            }
            LabeledContent("ios.custody.field.ciphertext_hash") {
                Text(verbatim: TransferFormat.shortDigest(receipt.ciphertextHash.rawValue))
                    .font(.caption.monospaced())
            }
            envelopeRow
            LabeledContent("ios.custody.field.received_at") {
                Text(verbatim: TransferFormat.captureDate(receipt.receivedAt))
            }
            LabeledContent("ios.custody.field.server") {
                Text(verbatim: receipt.serverID)
            }
            LabeledContent("ios.custody.field.server_key") {
                Text(verbatim: TransferFormat.fingerprint(receipt.serverKeyID))
                    .font(.caption.monospaced())
            }
            LabeledContent("ios.custody.field.sequence") {
                Text(verbatim: TransferFormat.count(Int(clamping: receipt.receiptSequence)))
            }
            priorRow
        } header: {
            Label("ios.custody.receipt.title", systemImage: "signature")
        } footer: {
            // The hash the *server* recomputed, never echoed from the client —
            // which is the whole reason the receipt can settle a dispute.
            Text("ios.custody.receipt.footer")
        }
    }

    /// Binds the receipt to the asset's provenance-chain position. Absent is a
    /// fact worth showing, not a blank row.
    @ViewBuilder
    private var envelopeRow: some View {
        LabeledContent("ios.custody.field.envelope_hash") {
            if let envelopeHash = receipt.envelopeHash {
                Text(verbatim: TransferFormat.shortDigest(envelopeHash))
                    .font(.caption.monospaced())
            } else {
                Text("ios.custody.field.absent")
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// `nil` only for the first receipt in the server's log — the append-only
    /// discipline of the provenance chain, applied to the server's own log.
    @ViewBuilder
    private var priorRow: some View {
        LabeledContent("ios.custody.field.prior_receipt") {
            if let priorReceiptHash = receipt.priorReceiptHash {
                Text(verbatim: TransferFormat.shortDigest(priorReceiptHash))
                    .font(.caption.monospaced())
            } else {
                Text("ios.custody.field.first_in_log")
                    .foregroundStyle(.secondary)
            }
        }
    }
}
