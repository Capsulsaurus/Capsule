import Foundation
import ManagedStore

/// An in-memory ``FileStore`` for tests, with targeted failure injection.
///
/// `MockFileStore` lets the import pipeline be tested without touching a real
/// disk, and — crucially — lets a test make one specific operation fail
/// (`injectFailure(on: .move)`) to verify the pipeline's two-phase-commit
/// rollback. It is an `actor`, so all state is serialised.
public actor MockFileStore: FileStore {
    /// A filesystem operation, named so a test can make exactly one fail.
    public enum Operation: Sendable, Hashable {
        case createDirectory, write, read, copy, move, remove, list, fileSize
    }

    /// Errors the mock raises.
    public enum MockError: Error, Sendable, Equatable {
        /// No file or directory exists at the URL.
        case notFound(URL)
        /// A destination URL is already occupied.
        case alreadyExists(URL)
        /// A failure deliberately injected for `operation`.
        case injected(Operation)
    }

    private var files: [URL: Data] = [:]
    private var directories: Set<URL> = []
    private var injectedFailures: [Operation: Error] = [:]

    public init() {}

    // MARK: Test configuration

    /// Place `data` at `url` as if it were already on disk, registering its
    /// parent directories.
    public func seedFile(_ data: Data, at url: URL) {
        let key = url.standardizedFileURL
        files[key] = data
        registerDirectory(key.deletingLastPathComponent())
    }

    /// Register `url` as an existing directory.
    public func seedDirectory(_ url: URL) {
        registerDirectory(url.standardizedFileURL)
    }

    /// Make every future `operation` throw — `MockError.injected` by default.
    public func injectFailure(on operation: Operation, error: Error? = nil) {
        injectedFailures[operation] = error ?? MockError.injected(operation)
    }

    /// Clear all injected failures.
    public func clearFailures() {
        injectedFailures.removeAll()
    }

    /// Every file URL currently stored.
    public func storedFileURLs() -> [URL] {
        Array(files.keys)
    }

    /// The number of files currently stored.
    public var fileCount: Int {
        files.count
    }

    /// The raw bytes at `url`, or `nil`.
    public func data(at url: URL) -> Data? {
        files[url.standardizedFileURL]
    }

    // MARK: FileStore

    public func fileExists(at url: URL) -> Bool {
        let key = url.standardizedFileURL
        return files[key] != nil || directories.contains(key)
    }

    public func createDirectory(at url: URL) throws {
        try failIfInjected(.createDirectory)
        registerDirectory(url.standardizedFileURL)
    }

    public func write(_ data: Data, to url: URL) throws {
        try failIfInjected(.write)
        let key = url.standardizedFileURL
        files[key] = data
        registerDirectory(key.deletingLastPathComponent())
    }

    public func read(at url: URL) throws -> Data {
        try failIfInjected(.read)
        guard let data = files[url.standardizedFileURL] else {
            throw MockError.notFound(url)
        }
        return data
    }

    public func copyItem(at source: URL, to destination: URL) throws {
        try failIfInjected(.copy)
        let sourceKey = source.standardizedFileURL
        let destinationKey = destination.standardizedFileURL
        guard let data = files[sourceKey] else { throw MockError.notFound(source) }
        guard files[destinationKey] == nil else { throw MockError.alreadyExists(destination) }
        files[destinationKey] = data
        registerDirectory(destinationKey.deletingLastPathComponent())
    }

    public func moveItem(at source: URL, to destination: URL) throws {
        try failIfInjected(.move)
        let sourceKey = source.standardizedFileURL
        let destinationKey = destination.standardizedFileURL
        guard let data = files[sourceKey] else { throw MockError.notFound(source) }
        guard files[destinationKey] == nil else { throw MockError.alreadyExists(destination) }
        files[destinationKey] = data
        files[sourceKey] = nil
        registerDirectory(destinationKey.deletingLastPathComponent())
    }

    public func removeItem(at url: URL) throws {
        try failIfInjected(.remove)
        let key = url.standardizedFileURL
        guard files[key] != nil || directories.contains(key) else {
            throw MockError.notFound(url)
        }
        files[key] = nil
        directories.remove(key)
    }

    public func contentsOfDirectory(at url: URL) throws -> [URL] {
        try failIfInjected(.list)
        let parent = url.standardizedFileURL
        let childFiles = files.keys.filter { $0.deletingLastPathComponent() == parent }
        let childDirectories = directories.filter { $0.deletingLastPathComponent() == parent }
        return Array(childFiles) + Array(childDirectories)
    }

    public func fileSize(at url: URL) throws -> Int64 {
        try failIfInjected(.fileSize)
        guard let data = files[url.standardizedFileURL] else {
            throw MockError.notFound(url)
        }
        return Int64(data.count)
    }

    // MARK: Helpers

    private func failIfInjected(_ operation: Operation) throws {
        if let error = injectedFailures[operation] { throw error }
    }

    /// Register `url` and every ancestor directory.
    private func registerDirectory(_ url: URL) {
        var current = url.standardizedFileURL
        while !directories.contains(current), current.pathComponents.count > 1 {
            directories.insert(current)
            current = current.deletingLastPathComponent()
        }
    }
}
