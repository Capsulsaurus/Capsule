import Foundation

// MARK: - ChangeBroadcaster

/// Fans one change notification out to every held stream.
///
/// Every port that mutates something exposes a `changes()` stream, and more than
/// one screen holds one at a time — a grid, a badge, a settings row. `AsyncStream`
/// is single-consumer, so something has to multiplex, and doing it once here is
/// what stops each port inventing its own subtly different version.
///
/// The subscriber token is a counter rather than a `UUID`, because this module
/// mints no random identifiers anywhere: a test that asserts on ordering should
/// not have to reason about whether the identity it saw was stable.
public actor ChangeBroadcaster<Element: Sendable> {
    private var continuations: [Int: AsyncStream<Element>.Continuation] = [:]
    private var nextToken = 0

    public init() {}

    /// A new stream.
    ///
    /// `nonisolated` on purpose: the port protocols declare `changes()` as a
    /// **synchronous** function, so there is no `await` available at the call
    /// site. Registration therefore hops onto the actor in a detached task, and
    /// the caller gets a live stream immediately. A change emitted in the same
    /// turn as the subscription can be missed — which is correct for a
    /// notification whose contract is "re-read the window you care about", and
    /// is why ``LibraryChange`` is not a diff.
    public nonisolated func subscribe() -> AsyncStream<Element> {
        AsyncStream(bufferingPolicy: .bufferingNewest(64)) { continuation in
            Task { await register(continuation) }
        }
    }

    /// Emit to every live stream.
    public func send(_ element: Element) {
        for continuation in continuations.values {
            continuation.yield(element)
        }
    }

    /// Finish every stream — what tearing a scenario down does.
    public func finish() {
        for continuation in continuations.values {
            continuation.finish()
        }
        continuations.removeAll()
    }

    /// How many streams are currently held. A test assertion, not a screen's
    /// business.
    public var subscriberCount: Int { continuations.count }

    private func register(_ continuation: AsyncStream<Element>.Continuation) {
        let token = nextToken
        nextToken += 1
        continuations[token] = continuation
        continuation.onTermination = { [weak self] _ in
            guard let self else { return }
            Task { await self.unregister(token) }
        }
    }

    private func unregister(_ token: Int) {
        continuations[token] = nil
    }
}
