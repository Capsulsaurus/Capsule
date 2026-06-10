import CryptoKit
import Foundation
import Testing

import CapsuleTestSupport
import ManagedStore

@Suite("ManagedLibraryLayout path computation")
struct ManagedLibraryLayoutTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/var/capsule/CapsuleLibrary"))

    @Test("media files are partitioned by the capture date's UTC month")
    func mediaPartitioning() {
        // 1_720_000_000 → 2024-07-03 12:26:40 UTC.
        let date = Date(timeIntervalSince1970: 1_720_000_000)
        let url = layout.mediaFile(uuid: "abc", fileExtension: "heic", captureDate: date)
        #expect(url.path.hasSuffix("/media/2024/2024-07/abc.heic"))
    }

    @Test("the sidecar is the media file's sibling with a .cbor extension")
    func sidecarSibling() {
        let date = Date(timeIntervalSince1970: 1_720_000_000)
        let media = layout.mediaFile(uuid: "abc", fileExtension: "heic", captureDate: date)
        let sidecar = layout.sidecarFile(forMediaFile: media)
        #expect(sidecar.lastPathComponent == "abc.cbor")
        #expect(sidecar.deletingLastPathComponent() == media.deletingLastPathComponent())
        #expect(sidecar == layout.sidecarFile(uuid: "abc", captureDate: date))
    }

    @Test("the skeleton directories all live under the root")
    func skeletonDirectories() {
        #expect(layout.skeletonDirectories.count == 4)
        for directory in layout.skeletonDirectories {
            #expect(directory.path.hasPrefix(layout.root.path))
        }
        #expect(layout.catalogFile.lastPathComponent == "catalog.sqlite")
        #expect(layout.catalogFile.deletingLastPathComponent() == layout.indexDirectory)
    }
}

@Suite("CryptoKitHasher SHA-256")
struct CryptoKitHasherTests {
    @Test("hashes a buffer to the published SHA-256 of \"abc\"")
    func knownAnswerABC() {
        let digest = CryptoKitHasher().hash(Data("abc".utf8))
        #expect(digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    }

    @Test("hashes the empty buffer to the published SHA-256")
    func knownAnswerEmpty() {
        let digest = CryptoKitHasher().hash(Data())
        #expect(digest == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    }

    @Test("streamed file hashing matches in-memory hashing for a large file")
    func streamedMatchesBuffer() async throws {
        let hasher = CryptoKitHasher()
        let bytes = Data((0 ..< 5_000_000).map { UInt8($0 & 0xFF) })
        let url = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try bytes.write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        #expect(try await hasher.hashFile(at: url) == hasher.hash(bytes))
    }
}

@Suite("SystemFileStore against a temporary directory")
struct SystemFileStoreTests {
    @Test("writes, reads, copies, moves, lists, and removes files")
    func roundTrip() async throws {
        let store = SystemFileStore()
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        try await store.createDirectory(at: root)

        let fileA = root.appending(path: "a.bin")
        let payload = Data("capsule".utf8)
        try await store.write(payload, to: fileA)
        #expect(await store.fileExists(at: fileA))
        #expect(try await store.read(at: fileA) == payload)
        #expect(try await store.fileSize(at: fileA) == Int64(payload.count))

        let fileB = root.appending(path: "b.bin")
        try await store.copyItem(at: fileA, to: fileB)
        #expect(await store.fileExists(at: fileB))

        let fileC = root.appending(path: "c.bin")
        try await store.moveItem(at: fileB, to: fileC)
        #expect(await store.fileExists(at: fileC))
        #expect(await store.fileExists(at: fileB) == false)

        #expect(try await store.contentsOfDirectory(at: root).count == 2)

        try await store.removeItem(at: fileA)
        #expect(await store.fileExists(at: fileA) == false)
    }
}

@Suite("MockFileStore")
struct MockFileStoreTests {
    @Test("stores and retrieves file data")
    func writeAndRead() async throws {
        let store = MockFileStore()
        let url = URL(filePath: "/lib/media/2024/x.heic")
        let payload = Data("bytes".utf8)
        try await store.write(payload, to: url)
        #expect(try await store.read(at: url) == payload)
        #expect(await store.fileExists(at: url))
        #expect(await store.fileCount == 1)
    }

    @Test("reading a missing file throws notFound")
    func missingFileThrows() async {
        let store = MockFileStore()
        await #expect(throws: MockFileStore.MockError.self) {
            _ = try await store.read(at: URL(filePath: "/nope"))
        }
    }

    @Test("an injected failure makes the targeted operation fail")
    func failureInjection() async throws {
        let store = MockFileStore()
        await store.injectFailure(on: .write)
        await #expect(throws: (any Error).self) {
            try await store.write(Data(), to: URL(filePath: "/lib/x"))
        }

        await store.clearFailures()
        try await store.write(Data("ok".utf8), to: URL(filePath: "/lib/x"))
        #expect(await store.fileCount == 1)
    }

    @Test("moveItem relocates data and clears the source")
    func moveRelocates() async throws {
        let store = MockFileStore()
        let source = URL(filePath: "/lib/.tmp/a")
        let destination = URL(filePath: "/lib/media/a")
        try await store.write(Data("payload".utf8), to: source)
        try await store.moveItem(at: source, to: destination)
        #expect(await store.fileExists(at: destination))
        #expect(await store.fileExists(at: source) == false)
    }
}
