package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.data.SyncError
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.isKnown
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import java.time.Clock
import java.time.Duration
import java.time.Instant

/**
 * The engine every fake repository is a thin face on.
 *
 * Three things live here and nowhere else, because each of them has to behave identically no
 * matter which of the sixteen contracts a caller came through.
 *
 * **The gate.** Latency and failure are injected in one place, so a scenario that says "the
 * account is past its grace window" refuses metadata growth on captions, tags, albums, people
 * *and* links without sixteen implementations having to remember to check.
 *
 * **The revision.** Writes bump a counter and every observed flow is derived from it, so a
 * mutation made through one repository is seen by a collector on another. Repositories that
 * each held their own `MutableStateFlow` would let a caption write leave the timeline stale.
 *
 * **The clock.** Taken as a parameter, never read from the system. Trash retention, session
 * expiry and stale-sync thresholds are all "how long since", and a mock that asked the wall
 * clock would produce a different screenshot every day.
 */
class MockBackend(
    /** The generated fixture underneath. */
    val library: MockLibrary = SeededLibrary.generate(),
    /** The clock every relative time is measured against. */
    val clock: Clock = Clock.fixed(LibrarySpec.DEFAULT_NOW, java.time.ZoneOffset.UTC),
    initialScenario: MockScenario = MockScenario.Default,
) {
    private val lock = Any()
    private val revision = MutableStateFlow(0L)
    private val scenarioState = MutableStateFlow(initialScenario)

    /** The mutable session state. */
    val state: MockState = MockState(library)

    /** The scenario in force, observable so a debug drawer can switch it live. */
    val scenario: StateFlow<MockScenario> get() = scenarioState.asStateFlow()

    /** Puts [next] in force. Every observed flow re-emits against it immediately. */
    fun applyScenario(next: MockScenario) {
        scenarioState.value = next
    }

    /** The current instant, from the injected clock. */
    fun now(): Instant = clock.instant()

    /** Runs [block] under the state lock and returns its result. */
    fun <T> read(block: (MockState) -> T): T = synchronized(lock) { block(state) }

    /**
     * Runs [block] under the state lock and bumps the revision.
     *
     * The bump is what makes a write observable. It happens after the mutation and outside the
     * lock's critical read, so a collector woken by it always sees the new state rather than
     * racing the writer that woke it.
     */
    fun <T> mutate(block: (MockState) -> T): T {
        val result = synchronized(lock) { block(state) }
        revision.value += 1
        return result
    }

    /** An opaque counter that changes whenever anything in [state] does. */
    fun revisions(): Flow<Long> = revision.asStateFlow()

    /**
     * A flow of [read]'s result, re-emitted on every write and on every scenario change.
     *
     * Deriving every observable from one counter rather than from per-collection flows is what
     * keeps "a write is seen by whoever is watching" true by construction instead of true
     * wherever someone remembered to wire it.
     */
    fun <T> observe(read: (MockState, MockScenario) -> T): Flow<T> =
        combine(revision, scenarioState) { _, current -> synchronized(lock) { read(state, current) } }
            .distinctUntilChanged()

    /**
     * Applies the scenario's latency, then its failures, before an operation runs.
     *
     * Latency first: an operation that is going to fail should still take as long to fail as it
     * would have taken to succeed, or every loading state the failure path needs would be
     * skipped over in a test that never sees it.
     */
    suspend fun gate(operation: MockOperation) {
        val current = scenarioState.value
        val latency = current.latency.forOperation(operation)
        if (!latency.isZero) delay(latency.toMillis())
        current.failures[operation]?.let { throw it }
        scenarioFailure(operation, current)?.let { throw it }
    }

    /** The failure [operation] must report under [scenario], or `null` when it may proceed. */
    private fun scenarioFailure(
        operation: MockOperation,
        scenario: MockScenario,
    ): CapsuleError? =
        when {
            operation.needsNetwork && scenario.connection == ConnectionClass.Offline -> LocalError.Offline
            operation.needsNetwork && !scenario.signedIn -> SyncError.Unauthenticated
            operation.needsNetwork && !scenario.protocolSupported -> ProtocolError.VersionUnsupported
            else -> quotaFailure(operation, scenario.quota.state)
        }

    /**
     * The quota refusal for [operation] at [quotaState].
     *
     * Follows the ladder exactly: a suspension stops uploads and new links, a hard-exceeded
     * state stops uploads, and only a grace-expired state reaches ordinary metadata growth.
     * Trash writes are never refused at any step — a state that blocked deletion would be a
     * trap with no exit.
     */
    private fun quotaFailure(
        operation: MockOperation,
        quotaState: QuotaState,
    ): CapsuleError? =
        when {
            operation == MockOperation.TrashWrite -> null
            quotaState == QuotaState.Suspended && operation.growsMetadata -> ModerationError.AccountSuspended
            operation == MockOperation.ImportRun && !quotaState.allowsUpload -> QuotaError.Exceeded
            operation.growsMetadata && !quotaState.allowsMetadataGrowth -> QuotaError.GraceLocked
            else -> null
        }

    /**
     * Refuses a write to an asset a newer `protocol_version` produced.
     *
     * Its enum tokens are outside this build's closed sets, so this client cannot re-sign the
     * sidecar without either dropping a field a peer wrote — which invalidates the signature —
     * or guessing at a value it does not understand.
     */
    fun requireWritable(asset: Asset) {
        val forward = !asset.assetType.isKnown || !asset.cull.isKnown || asset.captureTzSource?.isKnown == false
        if (forward) throw ProtocolError.VersionUnsupported
    }

    /** The asset under [id], or a local not-found. */
    fun requireAsset(id: AssetId): Asset = read { it.assets[id] } ?: throw LocalError.NotFound("asset ${id.value}")

    /** The album under [id], or a coded refusal a client can localize. */
    fun requireAlbum(id: com.justin13888.capsule.core.model.AlbumId): com.justin13888.capsule.core.model.Album =
        read { current -> current.albums.firstOrNull { it.id == id } } ?: throw AlbumError.NotAvailable

    /**
     * The quota status the current scenario implies.
     *
     * Derived from the generated usage rather than replacing it, so the bar's numbers still
     * agree with the number of photos above it in every state — a suspended account with an
     * empty-looking quota bar would look like two unrelated bugs.
     */
    fun quotaStatus(scenario: MockScenario = scenarioState.value): QuotaStatus {
        val base = library.quota
        return when (scenario.quota) {
            QuotaScenario.Ok -> base
            QuotaScenario.SoftWarning -> base.copy(used = base.softLimit + SOFT_OVERSHOOT_BYTES)
            QuotaScenario.HardExceeded ->
                base.copy(
                    used = (base.hardLimit ?: base.softLimit) + HARD_OVERSHOOT_BYTES,
                    hardLimitCrossedAt = now().minus(Duration.ofDays(1)),
                )

            QuotaScenario.GraceExpired ->
                base.copy(
                    used = (base.hardLimit ?: base.softLimit) + HARD_OVERSHOOT_BYTES,
                    hardLimitCrossedAt = now().minus(QuotaStatus.DEFAULT_GRACE_WINDOW).minus(Duration.ofDays(1)),
                )

            QuotaScenario.Suspended -> base.copy(isSuspended = true)
        }
    }

    /** Whether the library presents any content at all — `false` is the first-run state. */
    fun isPopulated(): Boolean = scenarioState.value.populated

    /** Every asset a default view may show, newest first, or nothing in the first-run state. */
    fun visibleAssets(current: MockState = state): List<Asset> =
        if (!isPopulated()) emptyList() else current.assets.values.filter(Asset::isInDefaultViews)

    private companion object {
        const val SOFT_OVERSHOOT_BYTES = 512L * 1024 * 1024
        const val HARD_OVERSHOOT_BYTES = 64L * 1024 * 1024
    }
}
