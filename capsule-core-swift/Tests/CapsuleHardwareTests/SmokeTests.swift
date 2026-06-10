import CryptoKit
import Foundation
import XCTest

@testable import CapsuleHardware

/// Proves the compiled `capsule-core` works when consumed from Swift over uniffi — the Apple
/// analogue of the Rust Linux software smoke. Run `./stage-bindings.sh` first.
final class SmokeTests: XCTestCase {
    private func freshRoot() throws -> String {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("capsule-swift-smoke-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    /// The pure-software path: the bindings link and a real workspace is created in Rust.
    func testSoftwarePathCreatesWorkspace() throws {
        let ws = try FfiWorkspace.create(
            root: try freshRoot(),
            passphrase: Data("correct horse".utf8),
            tier: .normal
        )
        XCTAssertFalse(try ws.userId().isEmpty)
        XCTAssertFalse(try ws.defaultAlbumId().isEmpty)
    }

    /// The full hardware-signer foreign-trait path, driven by the software Ed25519 signer (which
    /// produces genuine Ed25519, so it composes into the hybrid DSK end to end).
    func testSoftwareHardwareSignerRoundTrips() throws {
        let signer = SoftwareSigner(seed: Data(repeating: 7, count: 32))

        // Contract self-check before handing it to Rust.
        let pub = try signer.enroll(keyAlias: "device-dsk")
        XCTAssertEqual(pub.count, 32, "Ed25519 public key is 32 bytes")
        let sig = try signer.signClassical(keyAlias: "device-dsk", msg: Data("m".utf8))
        XCTAssertEqual(sig.count, 64, "Ed25519 signature is 64 bytes")
        XCTAssertThrowsError(try signer.assertNonExportable(keyAlias: "device-dsk")) { error in
            guard case HardwareSignerError.Exportable = error else {
                return XCTFail("software signer must report itself exportable")
            }
        }

        let ws = try FfiWorkspace.createWithHardwareSigner(
            root: try freshRoot(),
            passphrase: Data("correct horse".utf8),
            tier: .normal,
            hardware: signer,
            keyAlias: "device-dsk",
            mlSeed: Data(repeating: 9, count: 32)
        )
        XCTAssertFalse(try ws.userId().isEmpty)
    }

    /// The real Secure Enclave adapter. Skipped where no Secure Enclave is present (CI VMs); runs
    /// on Apple-Silicon / T2 Macs and devices. Verifies the P-256 key lifecycle + non-export.
    func testSecureEnclaveSignerOnDevice() throws {
        try XCTSkipUnless(SecureEnclave.isAvailable, "no Secure Enclave on this host")
        let se = SecureEnclaveSigner()
        let pub = try se.enroll(keyAlias: "se-dsk")
        XCTAssertEqual(pub.count, 65, "P-256 x9.63 public key is 65 bytes")
        let sig = try se.signClassical(keyAlias: "se-dsk", msg: Data("hello".utf8))
        XCTAssertEqual(sig.count, 64, "P-256 ECDSA r‖s is 64 bytes")
        XCTAssertNoThrow(try se.assertNonExportable(keyAlias: "se-dsk"))
    }
}
