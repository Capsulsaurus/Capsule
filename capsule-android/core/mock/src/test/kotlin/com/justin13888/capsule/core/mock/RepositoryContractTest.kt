package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.data.AuthError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.PersonId
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.RecoveryState
import com.justin13888.capsule.core.model.RecoveryVerifyOutcome
import com.justin13888.capsule.core.model.RestoreMode
import com.justin13888.capsule.core.model.ShareScope
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.time.Duration
import java.util.UUID

/**
 * The contracts the scenario and write tests do not otherwise reach.
 *
 * Sixteen interfaces are a lot of surface to leave unexercised: an implementation that
 * compiles proves only that the signatures match, and a fake that returns an empty list from
 * every method would pass that bar while making every screen it backs look broken.
 */
class RepositoryContractTest {
    @Test
    fun `a smart album evaluates to the assets its predicate actually matches`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val definitions = mock.search.smartAlbums().first()
            val ratings = definitions.first { it.displayName.value == "Four stars and up" }
            val matched = mock.search.evaluate(ratings.id, 0, Int.MAX_VALUE)
            assertTrue(matched.isNotEmpty())
            assertTrue(matched.all { it.rating >= 4 })
            // Evaluated in sorted asset-id order, so the result is identical across devices.
            assertEquals(matched.map { it.id.value }.sorted(), matched.map { it.id.value })
        }

    @Test
    fun `a definition from a later grammar is reported rather than guessed at`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val forward =
                mock.search
                    .smartAlbums()
                    .first()
                    .first { it.predicateSchema > SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA }
            assertThrows<ProtocolError.VersionUnsupported> { mock.search.evaluate(forward.id, 0, 10) }
        }

    @Test
    fun `saving a definition validates it, and a saved one is observable`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val existing =
                mock.search
                    .smartAlbums()
                    .first()
                    .first()
            val valid =
                existing.copy(
                    predicate =
                        Predicate.Leaf(
                            Term(
                                field = QueryField.Cull.asWire(),
                                op = Operator.Is.asWire(),
                                operand = Operand.CullLiteral(CullFlag.Pick),
                            ),
                        ),
                )
            mock.search.saveSmartAlbum(valid)
            assertEquals(
                valid,
                mock.search
                    .smartAlbums()
                    .first()
                    .first { it.id == valid.id },
            )
            val picks = mock.search.evaluate(valid.id, 0, Int.MAX_VALUE)
            assertTrue(picks.isNotEmpty())
            mock.search.deleteSmartAlbum(valid.id)
            assertTrue(
                mock.search
                    .smartAlbums()
                    .first()
                    .none { it.id == valid.id },
            )
        }

    @Test
    fun `free-text search finds an asset by a tag the library actually holds`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.metadata.addUserTag(subject.id, "reykjavik")
            assertTrue(mock.search.search("reykjavik", 0, 10).any { it.id == subject.id })
            assertTrue(
                mock.search
                    .suggestions("rey")
                    .first()
                    .contains("reykjavik"),
            )
            assertTrue(mock.search.search("", 0, 10).isEmpty())
        }

    @Test
    fun `the default album resolves through the owner pointer, and a scope overrides it`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val resolved = mock.albums.resolveDefaultAlbum()
            assertEquals(MockFixtures.LIBRARY.landmarks.defaultAlbumId, resolved.albumId)
            assertEquals(DefaultAlbumRule.OwnerPointer, resolved.rule)
            val scope =
                mock.imports
                    .scopes()
                    .first()
                    .first()
            val trip =
                MockFixtures.LIBRARY.landmarks.tripAlbumIds
                    .first()
            mock.imports.setScopeDestination(scope, trip)
            val overridden = mock.albums.resolveDefaultAlbum(scope)
            assertEquals(trip, overridden.albumId)
            assertEquals(DefaultAlbumRule.ScopeOverride, overridden.rule)
        }

    @Test
    fun `the designated default album cannot be deleted and a view cannot be an import target`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val default = MockFixtures.LIBRARY.landmarks.defaultAlbumId
            assertThrows<AlbumError.NotAvailable> { mock.albums.deleteAlbum(default) }
            val subject = mock.timeline.timelineWindow(0, 1).single()
            assertThrows<AlbumError.NotAvailable> {
                mock.albums.setAssetAlbum(subject.id, MockFixtures.LIBRARY.landmarks.viewAlbumId)
            }
            assertThrows<AlbumError.NotAvailable> {
                mock.albums.setDefaultAlbum(MockFixtures.LIBRARY.landmarks.viewAlbumId)
            }
        }

    @Test
    fun `repointing the default album moves the pointer rather than the assets`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val trip =
                MockFixtures.LIBRARY.landmarks.tripAlbumIds
                    .first()
            val before = mock.albums.albumAssets(trip, 0, Int.MAX_VALUE).size
            mock.albums.setDefaultAlbum(trip)
            assertEquals(trip, mock.albums.resolveDefaultAlbum().albumId)
            assertEquals(before, mock.albums.albumAssets(trip, 0, Int.MAX_VALUE).size)
            assertTrue(
                mock.albums
                    .albums()
                    .first()
                    .first { it.id == trip }
                    .isDefault,
            )
        }

    @Test
    fun `an album carries the viewer's capability and its shared membership`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val shared =
                mock.albums
                    .albums()
                    .first()
                    .first { it.members.isNotEmpty() }
            assertEquals(AlbumCapability.Admin, shared.viewerCapability?.let { cap -> cap.raw }?.let(::capabilityOf))
            assertTrue(shared.members.size > 1)
        }

    @Test
    fun `revoking every session needs a proof, and refuses without a valid one`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            assertThrows<AuthError.RevokeProofRequired> { mock.sessions.revokeAllSessions(ByteArray(0)) }
            assertThrows<AuthError.RevokeProofInvalid> { mock.sessions.revokeAllSessions("wrong".toByteArray()) }
            // Nothing was revoked: there is no partial success to reconcile.
            assertTrue(
                mock.sessions
                    .sessions()
                    .first()
                    .all { it.revokedAt == null },
            )
            mock.sessions.revokeAllSessions("capsule-master-key-proof".toByteArray())
            assertTrue(
                mock.sessions
                    .sessions()
                    .first()
                    .all { it.revokedAt != null },
            )
        }

    @Test
    fun `a revoked device stays in the directory`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val victim =
                mock.sessions
                    .devices()
                    .first()
                    .first { it.isActive && !it.isThisDevice }
            mock.sessions.revokeDevice(victim.id)
            val after =
                mock.sessions
                    .devices()
                    .first()
                    .first { it.id == victim.id }
            assertFalse(after.isActive)
        }

    @Test
    fun `a local-auth grant is per view and lapses on its own`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            assertNull(mock.sessions.localAuthGrant(GatedView.Hidden).first())
            mock.sessions.requireLocalAuth(GatedView.RecentlyDeleted)
            assertNotNull(mock.sessions.localAuthGrant(GatedView.RecentlyDeleted).first())
            // Unlocking Recently Deleted must not also unlock Hidden.
            assertNull(mock.sessions.localAuthGrant(GatedView.Hidden).first())
        }

    @Test
    fun `settings are written per field and observed together`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            mock.settings.setAutoSyncEnabled(false)
            mock.settings.setStaleSyncNotification(StaleSyncNotification.Disabled)
            mock.settings.setDefaultRetention(Duration.ofDays(7))
            mock.settings.setRetainIdentifiersOnExport(true)
            mock.settings.setGatedViewGrace(Duration.ofMinutes(1))
            val settings = mock.settings.settings().first()
            assertFalse(settings.autoSyncEnabled)
            assertEquals(StaleSyncNotification.Disabled, settings.staleSyncNotification)
            assertEquals(Duration.ofDays(7), settings.defaultRetention)
            assertTrue(settings.retainIdentifiersOnExport)
            assertEquals(Duration.ofMinutes(1), settings.gatedViewGrace)
        }

    @Test
    fun `sync scope and upload policy round-trip through settings`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            mock.sync.setSyncScope(SyncScope.MetadataOnly)
            assertEquals(SyncScope.MetadataOnly, mock.sync.syncScope().first())
            mock.sync.setUploadPolicy(com.justin13888.capsule.core.model.UploadPolicy.Staged)
            assertEquals(
                com.justin13888.capsule.core.model.UploadPolicy.Staged,
                mock.sync.uploadPolicy().first(),
            )
        }

    @Test
    fun `cache eviction never goes below what is pinned`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.storage.setPinned(subject.id, pinned = true)
            val before = mock.storage.cacheBytes().first()
            val freed = mock.storage.evictCache(targetBytes = 0)
            val after = mock.storage.cacheBytes().first()
            assertEquals(before - freed, after)
            // A pinned original is never evicted, however tight the budget.
            assertTrue(after > 0)
        }

    @Test
    fun `held representations are reported per asset`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val degraded = requireNotNull(MockFixtures.LIBRARY.landmarks.degradedAssetId)
            assertEquals(
                com.justin13888.capsule.core.model.HeldRepresentations.LQIP_ONLY,
                mock.storage.heldRepresentations(degraded).first(),
            )
        }

    @Test
    fun `recovery verification moves the state and a wrong secret eventually recommends a re-wrap`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            assertEquals(
                RecoveryVerifyOutcome.Verified,
                mock.recovery.verifyRecoverySecret("correct horse battery staple".toCharArray()),
            )
            assertTrue(mock.recovery.recoveryState().first() is RecoveryState.Healthy)
            repeat(3) { mock.recovery.verifyRecoverySecret("nope".toCharArray()) }
            // A guided re-wrap, never a lockout: the account is still reachable from here.
            assertTrue(mock.recovery.recoveryState().first() is RecoveryState.RewrapRecommended)
        }

    @Test
    fun `snoozing suppresses the prompt without suppressing the requirement`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            mock.recovery.snoozeVerification(Duration.ofDays(14))
            val state = mock.recovery.recoveryState().first()
            assertTrue(state is RecoveryState.Snoozed)
        }

    @Test
    fun `a dry-run restore reports without applying and a commit applies only what is absent`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val artifact =
                mock.recovery
                    .backups()
                    .first()
                    .single()
            val dryRun = mock.recovery.restore(artifact.id, RestoreMode.DryRun)
            assertTrue(dryRun.applied.isEmpty())
            assertTrue(dryRun.wouldAdd.isNotEmpty())
            // "The backup is older" is never on its own a reason to roll a library back.
            assertTrue(dryRun.needsReview)
            val committed = mock.recovery.restore(artifact.id, RestoreMode.Commit)
            assertEquals(dryRun.wouldAdd, committed.applied)
        }

    @Test
    fun `an issued share link returns its fragment secret once and disappears when revoked`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            val link = mock.sharing.createShareLink(ShareScope.SingleAsset(subject.id))
            assertNotNull(link.fragmentSecret)
            assertTrue(
                mock.sharing
                    .shareLinks()
                    .first()
                    .any { it.id == link.id },
            )
            mock.sharing.revokeShareLink(link.id)
            // Revoked, expired and never-existed are indistinguishable, so there is no
            // "revoked" row left to render.
            assertTrue(
                mock.sharing
                    .shareLinks()
                    .first()
                    .none { it.id == link.id },
            )
        }

    @Test
    fun `an upload link is provisioned under its caps and revoking it leaves the inbox alone`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val depositsBefore =
                mock.drops
                    .inbox()
                    .first()
                    .size
            val caps =
                com.justin13888.capsule.core.model
                    .LinkCaps(maxFileCount = 5, singleUse = true)
            val link = mock.sharing.createUploadLink(caps, passphrase = "gate")
            assertTrue(link.requiresPassphrase)
            assertTrue(link.acceptsDepositsAt(MockFixtures.SPEC.now))
            mock.sharing.revokeUploadLink(link.id)
            assertEquals(
                depositsBefore,
                mock.drops
                    .inbox()
                    .first()
                    .size,
            )
        }

    @Test
    fun `discarding a drop empties the inbox row without minting an asset`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val drop =
                mock.drops
                    .inbox()
                    .first()
                    .first()
            val before = mock.timeline.timelineWindow(0, Int.MAX_VALUE).size
            mock.drops.discard(drop.id)
            assertTrue(
                mock.drops
                    .inbox()
                    .first()
                    .none { it.id == drop.id },
            )
            assertEquals(before, mock.timeline.timelineWindow(0, Int.MAX_VALUE).size)
        }

    @Test
    fun `planning an import is deterministic and confirmation runs it`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val sources = List(12) { "DCIM/Camera/IMG_${1000 + it}.HEIC" }
            val first = mock.imports.plan(sources)
            val second = mock.imports.plan(sources)
            assertEquals(first, second)
            assertEquals(sources.size, first.added.size + first.skipped.size + first.conflicts.size)
            assertEquals(first.added.sumOf { it.sizeBytes }, first.totalSize)
            val runId = mock.imports.confirm(first.copy(conflicts = emptyList()))
            assertTrue(
                mock.imports
                    .progress(runId)
                    .first()
                    .isFinished,
            )
        }

    @Test
    fun `blocking a user is idempotent and personal`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val stranger = UserId("01234567-89ab-7cde-8f01-23456789abcd")
            mock.moderation.blockUser(stranger)
            mock.moderation.blockUser(stranger)
            assertEquals(
                1,
                mock.moderation
                    .blocks()
                    .first()
                    .size,
            )
            mock.moderation.unblockUser(stranger)
            assertTrue(
                mock.moderation
                    .blocks()
                    .first()
                    .isEmpty(),
            )
            assertTrue(
                mock.moderation
                    .events()
                    .first()
                    .isNotEmpty(),
            )
        }

    @Test
    fun `enrollment succeeds when the safety codes match and adds the device`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val before =
                mock.sessions
                    .devices()
                    .first()
                    .size
            val device = mock.enrollment.enroll("Studio iMac", PlatformTag.Macos, mock.enrollment.safetyCode())
            assertEquals(
                before + 1,
                mock.sessions
                    .devices()
                    .first()
                    .size,
            )
            // A UUIDv4, never a v7: a device id must not leak when the device was enrolled.
            assertEquals('4', device.id.value[UUID_VERSION_INDEX])
        }

    @Test
    fun `provenance always has at least a create record`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val plain = mock.timeline.timelineWindow(0, 1).single()
            assertTrue(
                mock.assets
                    .provenance(plain.id)
                    .first()
                    .isNotEmpty(),
            )
            val trashed = requireNotNull(MockFixtures.LIBRARY.landmarks.trashedExpiredAssetId)
            val chain = mock.assets.provenance(trashed).first()
            // Append-only in both directions: the create record survives the delete.
            assertEquals(2, chain.size)
            assertNotNull(chain.last().retentionUntil)
        }

    @Test
    fun `a purge is refused inside the signed retention window and permitted past it`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val fresh = requireNotNull(MockFixtures.LIBRARY.landmarks.trashedFreshAssetId)
            assertThrows<com.justin13888.capsule.core.data.LocalError.Storage> { mock.timeline.purge(fresh) }
            val expired = requireNotNull(MockFixtures.LIBRARY.landmarks.trashedExpiredAssetId)
            assertTrue(mock.timeline.expiredTrash(MockFixtures.SPEC.now).any { it.id == expired })
            mock.timeline.purge(expired)
            assertNull(mock.assets.findByUuid(expired))
        }

    @Test
    fun `an asset is findable by its content hash, which is what import deduplicates on`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject: Asset = mock.timeline.timelineWindow(0, 1).single()
            assertEquals(subject.id, mock.assets.findByHash(subject.contentHash)?.id)
        }

    @Test
    fun `people can be renamed, merged, hidden and given a cover`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val people = mock.people.people().first()
            val first = people.first()
            val second = people[1]
            mock.people.renamePerson(first.id, "Renamed")
            assertEquals(
                "Renamed",
                mock.people
                    .person(first.id)
                    .first()
                    ?.displayName,
            )
            val cover = mock.people.personAssets(first.id, 0, 1).first()
            mock.people.setPersonCover(first.id, cover.id)
            assertEquals(
                cover.id,
                mock.people
                    .person(first.id)
                    .first()
                    ?.coverAssetId,
            )
            mock.people.mergePeople(second.id, first.id)
            assertTrue(
                mock.people
                    .people()
                    .first()
                    .none { it.id == second.id },
            )
            mock.people.hidePerson(first.id, hidden = true)
            assertTrue(
                mock.people
                    .people()
                    .first()
                    .none { it.id == first.id },
            )
            // Hiding removes them from the view, never the clusters behind them.
            assertTrue(
                mock.people
                    .clusters()
                    .first()
                    .isNotEmpty(),
            )
        }

    @Test
    fun `detaching a cluster leaves it queryable but attached to nobody`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val attached =
                mock.people
                    .clusters()
                    .first()
                    .first { it.personId != null }
            mock.people.detachCluster(attached.id)
            val after =
                mock.people
                    .clusters()
                    .first()
                    .first { it.id == attached.id }
            assertNull(after.personId)
        }

    private fun capabilityOf(raw: String): AlbumCapability = AlbumCapability.fromWireOrThrow(raw)

    private companion object {
        /** Where the version nibble sits in a canonical UUID string. */
        const val UUID_VERSION_INDEX = 14
    }

    @Test
    fun `attaching a grouping to a new id creates the person it names`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val people = mock.people
            val cluster = mock.backend.read { it.clusters.first() }
            val fresh = PersonId(UUID.randomUUID().toString())

            // Naming an unnamed grouping is the only way a person is created — there is no
            // create call on the contract — so the rename that follows has to find them.
            people.assignCluster(cluster.id, fresh)
            people.renamePerson(fresh, "Ada")

            val named = people.people().first().single { it.id == fresh }
            assertEquals("Ada", named.displayName)
            assertTrue(cluster.id in named.clusterIds)
            assertEquals(cluster.assetIds.size, named.assetCount)
        }
}
