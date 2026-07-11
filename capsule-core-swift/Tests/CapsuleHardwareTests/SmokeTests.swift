import CryptoKit
import Foundation
import Testing

@testable import CapsuleHardware

/// Proves the compiled `capsule-core` works when consumed from Swift over uniffi — the Apple
/// analogue of the Rust Linux software smoke. Run `./stage-bindings.sh` first.
@Suite("capsule-core Swift FFI smoke")
struct SmokeTests {
    /// This harness's self-reported build identity (S-D15), stamped into the manifests the
    /// workspace authors.
    private let client = FfiClientBuild(clientId: "capsule-core-swift", semver: "0.0.0")

    private func freshRoot() throws -> String {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("capsule-swift-smoke-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    /// Write a minimal JPEG-magic file and return its path.
    private func freshImage() throws -> String {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("capsule-swift-img-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let img = dir.appendingPathComponent("photo.jpg")
        try Data([0xFF, 0xD8, 0xFF] + Array("se p256 smoke bytes".utf8)).write(to: img)
        return img.path
    }

    /// Drive a full offline lifecycle (create → import → verify → read) over a P-256
    /// hardware-composed workspace, returning the verify outcome and the read-back bytes. Runs on
    /// the large stack because workspace creation triggers ML-DSA-65 key generation.
    private func p256RoundTrip(_ hardware: HardwareSigner) throws -> (FfiVerifyOutcome, Data) {
        try onLargeStack {
            let workspace = try FfiWorkspace.createWithP256HardwareSigner(
                root: try self.freshRoot(),
                passphrase: Data("correct horse".utf8),
                tier: .normal,
                hardware: hardware,
                keyAlias: "device-dsk",
                mlSeed: Data(repeating: 9, count: 32),
                client: self.client
            )
            // Import into a session album (the default album has no session key material minted).
            let album = try workspace.createAlbum(name: "P-256 Trip")
            let asset = try workspace.importAsset(albumId: album, src: try self.freshImage())
            let outcome = try workspace.verify(assetId: asset)
            let bytes = try workspace.readPlaintext(assetId: asset)
            return (outcome, bytes)
        }
    }

    /// Run `body` on a dedicated thread with an ample stack and return its result.
    ///
    /// swift-testing executes test bodies on the Swift-concurrency cooperative pool, whose worker
    /// threads carry a small stack. `capsule-core`'s ML-DSA-65 key generation expands a large
    /// lattice matrix on the stack and overflows it (SIGBUS at the stack-guard page). XCTest ran
    /// tests on the main thread's 8 MiB stack, so this never surfaced. Confine the FFI work that
    /// drives key generation to a thread with a generous stack; the assertions stay on the test
    /// thread so `#expect`/`#require` still record against the running test.
    private func onLargeStack<T>(_ body: @escaping () throws -> T) throws -> T {
        let done = DispatchSemaphore(value: 0)
        var result: Result<T, Error>!
        let thread = Thread {
            result = Result { try body() }
            done.signal()
        }
        thread.stackSize = 64 * 1024 * 1024
        thread.start()
        done.wait()
        return try result.get()
    }

    /// The pure-software path: the bindings link and a real workspace is created in Rust.
    @Test("the software path links the bindings and creates a workspace")
    func softwarePathCreatesWorkspace() throws {
        let (userId, defaultAlbumId) = try onLargeStack {
            let workspace = try FfiWorkspace.create(
                root: try self.freshRoot(),
                passphrase: Data("correct horse".utf8),
                tier: .normal,
                client: self.client
            )
            return (try workspace.userId(), try workspace.defaultAlbumId())
        }
        #expect(!userId.isEmpty)
        #expect(!defaultAlbumId.isEmpty)
    }

    /// The full hardware-signer foreign-trait path, driven by the software Ed25519 signer (which
    /// produces genuine Ed25519, so it composes into the hybrid DSK end to end).
    @Test("the software hardware-signer path round-trips through Rust")
    func softwareHardwareSignerRoundTrips() throws {
        let signer = SoftwareSigner(seed: Data(repeating: 7, count: 32))

        // Contract self-check before handing it to Rust.
        let pub = try signer.enroll(keyAlias: "device-dsk")
        #expect(pub.count == 32, "Ed25519 public key is 32 bytes")
        let sig = try signer.signClassical(keyAlias: "device-dsk", msg: Data("m".utf8))
        #expect(sig.count == 64, "Ed25519 signature is 64 bytes")
        #expect("software signer must report itself exportable") {
            try signer.assertNonExportable(keyAlias: "device-dsk")
        } throws: { error in
            guard case HardwareSignerError.Exportable = error else { return false }
            return true
        }

        let userId = try onLargeStack {
            let workspace = try FfiWorkspace.createWithHardwareSigner(
                root: try self.freshRoot(),
                passphrase: Data("correct horse".utf8),
                tier: .normal,
                hardware: signer,
                keyAlias: "device-dsk",
                mlSeed: Data(repeating: 9, count: 32),
                client: self.client
            )
            return try workspace.userId()
        }
        #expect(!userId.isEmpty)
    }

    /// The software **P-256** element composed through `P256HybridSigningKey` into a real
    /// workspace — the always-runnable half of the S-F2 smoke (a CI VM has no Secure Enclave). It
    /// mirrors ``SecureEnclaveSigner``'s wire contract (65-byte x9.63 public key, DER ECDSA
    /// signature), so a green run here proves the whole P-256 FFI composition independent of
    /// hardware. Import + verify exercises sign/verify of a manifest through the composed key.
    @Test("the software P-256 element composes through the P-256 hybrid FFI path")
    func softwareP256ComposesEndToEnd() throws {
        let signer = SoftwareP256Signer(seed: Data(repeating: 7, count: 32))

        // Contract self-check before handing it to Rust: SEC1 public key, DER signature, honest
        // non-exportability.
        let pub = try signer.enroll(keyAlias: "device-dsk")
        #expect(pub.count == 65, "P-256 x9.63 (uncompressed SEC1) public key is 65 bytes")
        #expect(pub.first == 0x04, "uncompressed-point tag")
        let sig = try signer.signClassical(keyAlias: "device-dsk", msg: Data("m".utf8))
        #expect(sig.first == 0x30, "DER SEQUENCE tag — not raw r‖s")
        #expect("software signer must report itself exportable") {
            try signer.assertNonExportable(keyAlias: "device-dsk")
        } throws: { error in
            guard case HardwareSignerError.Exportable = error else { return false }
            return true
        }

        let (outcome, bytes) = try p256RoundTrip(signer)
        #expect({ if case .accept = outcome { return true } else { return false } }(),
                 "a manifest signed by the P-256 hybrid must verify through verify_asset")
        #expect(bytes == Data([0xFF, 0xD8, 0xFF] + Array("se p256 smoke bytes".utf8)))
    }

    /// The real Secure Enclave adapter composed through the P-256 hybrid, end to end. Skipped where
    /// no Secure Enclave is present (CI VMs); runs on Apple-Silicon / T2 Macs and devices. The
    /// device directory and the imported asset's manifest are signed by the SE-held P-256 key + the
    /// software ML-DSA-65 half; verify proves both halves check, and `assertNonExportable` confirms
    /// the key never left the enclave.
    @Test(
        "the real Secure Enclave adapter composes through the P-256 hybrid FFI path",
        .enabled(if: SecureEnclave.isAvailable, "no Secure Enclave on this host")
    )
    func secureEnclaveComposesEndToEnd() throws {
        let enclave = SecureEnclaveSigner()
        let (outcome, bytes) = try p256RoundTrip(enclave)
        #expect({ if case .accept = outcome { return true } else { return false } }(),
                 "a manifest signed by the SE-composed P-256 hybrid must verify")
        #expect(bytes == Data([0xFF, 0xD8, 0xFF] + Array("se p256 smoke bytes".utf8)))
        // The private key never leaves the enclave — a throw here fails the test.
        try enclave.assertNonExportable(keyAlias: "device-dsk")
    }

    /// The software **P-256 ECDH** element composed through the hybrid DEK — the always-runnable half
    /// of the S-F5 smoke (a CI VM has no Secure Enclave). It mirrors ``SecureEnclaveKeyAgreement``'s
    /// wire contract (65-byte x9.63 public key, raw 32-byte ECDH secret), so a green run here proves
    /// the whole P-256 hybrid-DEK composition independent of hardware: a secret encapsulated to the
    /// published hybrid public key is recovered by decapsulating through the element's ECDH.
    @Test("the software P-256 element composes through the hybrid DEK FFI path")
    func softwareP256KeyAgreementComposesEndToEnd() throws {
        let element = SoftwareP256KeyAgreement(seed: Data(repeating: 7, count: 32))

        // Contract self-check before handing it to Rust: SEC1 public key, 32-byte ECDH secret,
        // honest non-exportability.
        let pub = try element.enroll(keyAlias: "device-dek")
        #expect(pub.count == 65, "P-256 x9.63 (uncompressed SEC1) public key is 65 bytes")
        #expect(pub.first == 0x04, "uncompressed-point tag")
        let secret = try element.keyAgreement(keyAlias: "device-dek", peerPublic: pub)
        #expect(secret.count == 32, "raw P-256 ECDH secret is the 32-byte x-coordinate")
        #expect("software element must report itself exportable") {
            try element.assertNonExportable(keyAlias: "device-dek")
        } throws: { error in
            guard case HardwareSignerError.Exportable = error else { return false }
            return true
        }

        let matched = try onLargeStack {
            try p256HardwareDekRoundTrip(
                hardware: element,
                keyAlias: "device-dek",
                mlSeed: Data(repeating: 9, count: 32)
            )
        }
        #expect(matched, "the P-256 hybrid DEK must recover the encapsulated secret via ECDH")
    }

    /// The real Secure Enclave ECDH element composed through the hybrid DEK, end to end. Skipped where
    /// no Secure Enclave is present (CI VMs); runs on Apple-Silicon / T2 Macs and devices. The DEK's
    /// classical half is an SE-held P-256 key; the shared secret is recovered by ECDH inside the
    /// enclave, and `assertNonExportable` confirms the key never left it.
    @Test(
        "the real Secure Enclave element composes through the hybrid DEK FFI path",
        .enabled(if: SecureEnclave.isAvailable, "no Secure Enclave on this host")
    )
    func secureEnclaveKeyAgreementComposesEndToEnd() throws {
        let enclave = SecureEnclaveKeyAgreement()
        let matched = try onLargeStack {
            try p256HardwareDekRoundTrip(
                hardware: enclave,
                keyAlias: "device-dek",
                mlSeed: Data(repeating: 9, count: 32)
            )
        }
        #expect(matched, "the SE-composed P-256 hybrid DEK must recover the encapsulated secret")
        // The private key never leaves the enclave — a throw here fails the test.
        try enclave.assertNonExportable(keyAlias: "device-dek")
    }
}
