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

    /// The real Secure Enclave adapter. Skipped where no Secure Enclave is present (CI VMs); runs
    /// on Apple-Silicon / T2 Macs and devices. Verifies the P-256 key lifecycle + non-export.
    @Test(
        "the real Secure Enclave adapter round-trips its P-256 key lifecycle",
        .enabled(if: SecureEnclave.isAvailable, "no Secure Enclave on this host")
    )
    func secureEnclaveSignerOnDevice() throws {
        let enclave = SecureEnclaveSigner()
        let pub = try enclave.enroll(keyAlias: "se-dsk")
        #expect(pub.count == 65, "P-256 x9.63 public key is 65 bytes")
        let sig = try enclave.signClassical(keyAlias: "se-dsk", msg: Data("hello".utf8))
        #expect(sig.count == 64, "P-256 ECDSA r‖s is 64 bytes")
        // A throw here fails the test — the swift-testing analogue of XCTAssertNoThrow.
        try enclave.assertNonExportable(keyAlias: "se-dsk")
    }
}
