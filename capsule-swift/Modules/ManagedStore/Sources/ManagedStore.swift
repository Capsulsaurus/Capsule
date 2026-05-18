import Foundation

/// The Capsule-managed, on-disk photo library.
///
/// Owns everything platform-specific about the managed store: the iOS-sandbox
/// directory layout, atomic file copy/rename, streamed `CryptoKit` SHA-256
/// hashing, sidecar file I/O, and the 4-phase import pipeline. All catalog
/// reads/writes are delegated to the Rust core via `CapsuleCatalog`.
///
/// - Note: implemented in Phase 4.
public enum ManagedStore {}
