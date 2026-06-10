import Foundation

/// The filesystem operations the managed store and import pipeline depend on.
///
/// Abstracting the filesystem behind this protocol lets the import pipeline be
/// tested deterministically against an in-memory `MockFileStore`, including
/// injected failures (a write that throws mid-import) that are impractical to
/// provoke against a real disk. ``SystemFileStore`` is the production conformer.
///
/// Methods are `async` so the import actor runs blocking I/O off the main
/// thread. Paths are file URLs.
public protocol FileStore: Sendable {
    /// Whether a file or directory exists at `url`.
    func fileExists(at url: URL) async -> Bool

    /// Create the directory at `url`, including any missing parents. A no-op
    /// if it already exists.
    func createDirectory(at url: URL) async throws

    /// Atomically write `data` to `url`, replacing any existing file.
    func write(_ data: Data, to url: URL) async throws

    /// Read the entire file at `url`.
    func read(at url: URL) async throws -> Data

    /// Copy the file at `source` to `destination`.
    func copyItem(at source: URL, to destination: URL) async throws

    /// Move (rename) the item at `source` to `destination`. On the same
    /// volume — the managed store case — this is atomic.
    func moveItem(at source: URL, to destination: URL) async throws

    /// Delete the item at `url`. Throws if it does not exist.
    func removeItem(at url: URL) async throws

    /// The direct contents of the directory at `url`, as file URLs.
    func contentsOfDirectory(at url: URL) async throws -> [URL]

    /// The size in bytes of the file at `url`.
    func fileSize(at url: URL) async throws -> Int64
}

/// The production ``FileStore`` — a thin, `Sendable` façade over `FileManager`.
///
/// `FileManager.default` is safe for concurrent use across these operations;
/// this type is stateless, so callers may share or copy it freely.
public struct SystemFileStore: FileStore {
    public init() {}

    public func fileExists(at url: URL) async -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    public func createDirectory(at url: URL) async throws {
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }

    public func write(_ data: Data, to url: URL) async throws {
        try data.write(to: url, options: .atomic)
    }

    public func read(at url: URL) async throws -> Data {
        try Data(contentsOf: url)
    }

    public func copyItem(at source: URL, to destination: URL) async throws {
        try FileManager.default.copyItem(at: source, to: destination)
    }

    public func moveItem(at source: URL, to destination: URL) async throws {
        try FileManager.default.moveItem(at: source, to: destination)
    }

    public func removeItem(at url: URL) async throws {
        try FileManager.default.removeItem(at: url)
    }

    public func contentsOfDirectory(at url: URL) async throws -> [URL] {
        try FileManager.default.contentsOfDirectory(
            at: url,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        )
    }

    public func fileSize(at url: URL) async throws -> Int64 {
        let values = try url.resourceValues(forKeys: [.fileSizeKey])
        return Int64(values.fileSize ?? 0)
    }
}
