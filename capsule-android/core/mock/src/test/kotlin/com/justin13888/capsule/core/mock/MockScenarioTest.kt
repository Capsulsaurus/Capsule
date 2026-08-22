package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.data.BlobError
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.EnrollmentError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.data.ShareError
import com.justin13888.capsule.core.data.SyncError
import com.justin13888.capsule.core.data.UploadError
import com.justin13888.capsule.core.mock.image.ProceduralImageRequest
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.time.Duration

/**
 * Every scenario preset, driven to the outcome it documents.
 *
 * The point is not that each toggle works in isolation — it is that a preset a screenshot test
 * or a debug drawer selects by name reaches a *specific*, named condition, with the `error.*`
 * code the real server would send. A preset whose outcome nobody asserts is a preset that
 * quietly stops working.
 */
class MockScenarioTest {
    private fun repositories(scenario: MockScenario) = MockFixtures.freshRepositories(scenario)

    /** The id every preset in [MockScenario.ALL] is checked to be distinct by. */
    @Test
    fun `every preset is enumerated and uniquely named`() {
        assertEquals(
            MockScenario.ALL.size,
            MockScenario.ALL
                .map(MockScenario::id)
                .distinct()
                .size,
        )
        assertTrue(MockScenario.ALL.contains(MockScenario.Default))
    }

    @Test
    fun `default succeeds and records a completed sync`() =
        runTest {
            val mock = repositories(MockScenario.Default)
            mock.sync.forceSyncNow()
            assertEquals(MockFixtures.SPEC.now, mock.sync.lastCompletedSync().first())
        }

    @Test
    fun `offline refuses anything remote and still serves the gallery`() =
        runTest {
            val mock = repositories(MockScenario.Offline)
            val failure = assertThrows<LocalError.Offline> { mock.sync.forceSyncNow() }
            // A local failure carries no catalogue code, and saying so is honest rather than
            // inventing a key the catalogue does not hold.
            assertNull(failure.code)
            // Reads keep working: a gallery read path performs no network I/O at all.
            assertTrue(mock.timeline.timelineWindow(0, 10).isNotEmpty())
            assertEquals(ConnectionClass.Offline, mock.sync.connectionClass().first())
        }

    @Test
    fun `never signed in refuses remote work with the sync unauthenticated code`() =
        runTest {
            val mock = repositories(MockScenario.NeverSignedIn)
            val refusal = assertThrows<SyncError.Unauthenticated> { mock.sync.forceSyncNow() }
            assertCode("error.sync.unauthenticated", refusal)
            assertNull(mock.sessions.currentSession().first())
        }

    @Test
    fun `first run shows an empty library that still has its default container`() =
        runTest {
            val mock = repositories(MockScenario.FirstRun)
            assertTrue(mock.timeline.timelineWindow(0, 10).isEmpty())
            val albums = mock.albums.albums().first()
            assertEquals(1, albums.size)
            assertTrue(albums.single().isDefault)
        }

    @Test
    fun `a soft warning still admits uploads`() =
        runTest {
            val mock = repositories(MockScenario.QuotaSoftWarning)
            val quota = mock.storage.quota().first()
            assertEquals(QuotaState.SoftWarning, quota.stateAt(MockFixtures.SPEC.now))
            val plan = mock.imports.plan(listOf("DCIM/Camera/IMG_0001.HEIC"))
            mock.imports.confirm(plan)
        }

    @Test
    fun `a hard-exceeded quota refuses a new upload session`() =
        runTest {
            val mock = repositories(MockScenario.QuotaExhausted)
            val plan = mock.imports.plan(listOf("DCIM/Camera/IMG_0001.HEIC"))
            assertCode("error.quota.exceeded", assertThrows<QuotaError.Exceeded> { mock.imports.confirm(plan) })
            // Reads and deletes keep working — a state that blocked deletion would be a trap.
            assertTrue(mock.timeline.timelineWindow(0, 1).isNotEmpty())
        }

    @Test
    fun `a grace-expired quota refuses metadata growth but never a delete`() =
        runTest {
            val mock = repositories(MockScenario.QuotaGraceExpired)
            val subject = mock.timeline.timelineWindow(0, 1).single()
            assertCode(
                "error.quota.grace_locked",
                assertThrows<QuotaError.GraceLocked> { mock.metadata.setCaption(subject.id, "blocked") },
            )
            // The way back under quota must stay open.
            mock.timeline.softDelete(subject.id, MockFixtures.SPEC.now.plus(Duration.ofDays(30)))
        }

    @Test
    fun `a suspended account keeps reads and loses uploads`() =
        runTest {
            val mock = repositories(MockScenario.AccountSuspended)
            val plan = mock.imports.plan(listOf("DCIM/Camera/IMG_0002.HEIC"))
            assertCode(
                "error.moderation.account_suspended",
                assertThrows<ModerationError.AccountSuspended> { mock.imports.confirm(plan) },
            )
            val effects = mock.moderation.suspensionEffects().first()
            assertNotNull(effects)
            assertTrue(effects!!.canRead)
            // Master-key-gated revocation survives, so a compromised-and-suspended account can
            // still lock the attacker out.
            assertTrue(effects.canRevokeAllSessions)
        }

    @Test
    fun `an original still on its device reports a transient pending code`() =
        runTest {
            val mock = repositories(MockScenario.AwaitingOriginal)
            val subject = mock.timeline.timelineWindow(0, 1).single()
            assertCode(
                "error.blob.pending_upload",
                assertThrows<BlobError.PendingUpload> {
                    mock.sync.ensureRepresentation(subject.id, RepresentationTier.Original)
                },
            )
            // Explicitly not a durability loss: the local copy must be kept.
            assertTrue(!mock.storage.verifyDurable(subject.id))
        }

    @Test
    fun `a gone original degrades instead of failing`() =
        runTest {
            val mock = repositories(MockScenario.GoneDegrade)
            val subject = mock.timeline.timelineWindow(0, 1).single()
            val served = mock.sync.ensureRepresentation(subject.id, RepresentationTier.Original)
            // Degradation walks down the ladder and stops at the floor; the asset stays listed.
            assertTrue(served.rank < RepresentationTier.Original.rank)
            assertTrue(subject.heldRepresentations.holds(served))
        }

    @Test
    fun `a forbidden fetch surfaces as a membership problem, not a missing file`() =
        runTest {
            val mock = repositories(MockScenario.MembershipResync)
            val subject = mock.timeline.timelineWindow(0, 1).single()
            assertCode(
                "error.album.not_available",
                assertThrows<AlbumError.NotAvailable> {
                    mock.sync.ensureRepresentation(subject.id, RepresentationTier.Original)
                },
            )
        }

    @Test
    fun `a replayed manifest is quarantined rather than applied or dropped`() =
        runTest {
            val mock = repositories(MockScenario.StaleRevival)
            val refusal = assertThrows<UploadError.StaleRevival> { mock.sync.forceSyncNow() }
            assertCode("error.upload.stale_revival", refusal)
            val subject = requireNotNull(MockFixtures.LIBRARY.landmarks.quarantinedAssetId)
            val state = mock.sync.assetSyncState(subject).first()
            assertTrue(state is com.justin13888.capsule.core.model.SyncState.Quarantined)
        }

    @Test
    fun `a sandboxed decode failure returns nothing to draw rather than throwing`() =
        runTest {
            val mock = repositories(MockScenario.SandboxDecodeFailure)
            val subject = mock.timeline.timelineWindow(0, 1).single()
            val request =
                ProceduralImageRequest(
                    assetId = subject.id,
                    lqip = subject.lqip,
                    aspectRatio = subject.dimensions?.aspectRatio ?: 1f,
                    tier = RepresentationTier.Thumbnail,
                    longEdgePx = 64,
                )
            assertNull(mock.images.decode(request))
            // The same request decodes fine once the sandbox is healthy again.
            mock.applyScenario(MockScenario.Default)
            assertNotNull(mock.images.decode(request))
        }

    @Test
    fun `an asset from a newer schema refuses a write`() =
        runTest {
            val mock = repositories(MockScenario.ForwardSchema)
            val subject = requireNotNull(MockFixtures.LIBRARY.landmarks.forwardSchemaAssetId)
            assertCode(
                "error.protocol.version_unsupported",
                assertThrows<ProtocolError.VersionUnsupported> { mock.assets.setRating(subject, 4) },
            )
            // Reading it is fine — the unknown tokens are carried, not acted on.
            assertNotNull(mock.assets.findByUuid(subject))
        }

    @Test
    fun `a safety-code mismatch refuses the enrollment`() =
        runTest {
            val mock = repositories(MockScenario.EnrollmentMitm)
            assertCode(
                "error.enrollment.code_refused",
                assertThrows<EnrollmentError.CodeRefused> {
                    mock.enrollment.enroll("New phone", PlatformTag.Android, mock.enrollment.safetyCode())
                },
            )
        }

    @Test
    fun `a protocol mismatch tells the user to update`() =
        runTest {
            val mock = repositories(MockScenario.ProtocolMismatch)
            assertCode(
                "error.protocol.version_unsupported",
                assertThrows<ProtocolError.VersionUnsupported> { mock.sync.forceSyncNow() },
            )
            assertThrows<ProtocolError.VersionUnsupported> {
                mock.enrollment.enroll("New phone", PlatformTag.Android, mock.enrollment.safetyCode())
            }
        }

    @Test
    fun `a fortnight-stale library reports its last completed sync`() =
        runTest {
            val mock = repositories(MockScenario.StaleSync)
            val last = requireNotNull(mock.sync.lastCompletedSync().first())
            val behind = Duration.between(last, MockFixtures.SPEC.now)
            assertTrue(behind >= com.justin13888.capsule.core.model.StaleSyncPolicy.STALE_AFTER, "only $behind behind")
        }

    @Test
    fun `a degraded link reports an adverse connection class`() =
        runTest {
            val mock = repositories(MockScenario.SyncDegraded)
            assertEquals(ConnectionClass.Adverse, mock.sync.connectionClass().first())
            // Adverse is derived from behaviour, never reported by the OS, and it is still a
            // usable path — bulk work waits, everything else does not.
            assertTrue(ConnectionClass.Adverse.isUsable)
            assertTrue(!ConnectionClass.Adverse.allowsBulk)
        }

    @Test
    fun `a per-operation failure can be injected without a preset`() =
        runTest {
            val mock = repositories(MockScenario.Default.failing(MockOperation.Search, ShareError.RateLimited))
            val refusal = assertThrows<ShareError.RateLimited> { mock.search.search("a", 0, 5) }
            assertCode("error.share.rate_limited", refusal)
        }

    @Test
    fun `latency is applied before the answer`() =
        runTest {
            val mock = repositories(MockScenario.Realistic)
            val started = currentTimeMillisVirtual()
            mock.timeline.timelineWindow(0, 5)
            assertTrue(currentTimeMillisVirtual() > started, "no virtual time elapsed")
        }

    /**
     * The test dispatcher's virtual clock, which `delay` advances without really waiting.
     *
     * Injected latency is real `delay`, so a scenario that makes a screen slow costs a test
     * nothing — which is what lets the slow presets stay in the suite rather than being
     * quietly dropped for making it drag.
     */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private fun kotlinx.coroutines.test.TestScope.currentTimeMillisVirtual(): Long = testScheduler.currentTime

    private fun assertCode(
        expected: String,
        error: CapsuleError,
    ) = assertEquals(ErrorCode(expected), error.code)

    @Test
    fun `every scenario preset is reachable through the backend`() =
        runTest {
            // Selecting a preset must never be what breaks: the debug drawer applies them by
            // name, and a preset that throws on selection is worse than one that does nothing.
            val mock = repositories(MockScenario.Default)
            MockScenario.ALL.forEach { scenario ->
                mock.applyScenario(scenario)
                assertEquals(scenario, mock.backend.scenario.value)
                runCatching { mock.timeline.timelineWindow(0, 1) }
            }
        }
}
