package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Nested
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import kotlin.math.abs

/**
 * The derived projections the UI reads: default-view eligibility, privacy-on-export
 * redaction, colour and geometry encodings, and link caps.
 *
 * Every one of them is computed rather than stored, so each is a place where a second
 * source of truth would have been easy and wrong.
 */
class ProjectionTest {
    @Nested
    inner class DefaultViewEligibility {
        @Test
        fun `an ordinary asset appears in default views`() {
            assertTrue(asset().isInDefaultViews)
        }

        @Test
        fun `a hidden asset does not`() {
            assertFalse(asset().copy(isHidden = true).isInDefaultViews)
        }

        @Test
        fun `a trashed asset does not`() {
            val trashed = asset().copy(deletion = DeletionState.Trashed(T0, T0.plus(Duration.ofDays(30))))
            assertFalse(trashed.isInDefaultViews)
        }

        @Test
        fun `a suppressed stack member does not`() {
            // The JPEG half of a RAW+JPEG pair; suppressed by stacking rather than by the
            // user, which is why it is a separate flag from `hidden`.
            val suppressed = asset().copy(stack = membership(isSuppressed = true))
            assertFalse(suppressed.isInDefaultViews)
        }

        @Test
        fun `a stack primary does appear`() {
            assertTrue(asset().copy(stack = membership(isSuppressed = false)).isInDefaultViews)
        }

        @Test
        fun `all three exclusions are checked, not just the first`() {
            // A caller that forgot one would leak content the user asked to keep out of sight.
            val everything =
                asset().copy(
                    isHidden = true,
                    deletion = DeletionState.Trashed(T0, T0.plus(Duration.ofDays(30))),
                    stack = membership(isSuppressed = true),
                )
            assertFalse(everything.isInDefaultViews)
        }

        private fun membership(isSuppressed: Boolean) =
            StackMembership(
                stackId = StackId("st-1"),
                stackType = StackType.RawJpeg.asWire(),
                role = if (isSuppressed) StackRole.Member.asWire() else StackRole.Primary.asWire(),
                isSuppressed = isSuppressed,
            )
    }

    @Nested
    inner class PrivacyOnExport {
        @Test
        fun `a camera serial is stripped while the model survives`() {
            // A serial links every photo to one physical unit; a model name identifies a
            // product line, which is not fingerprinting surface.
            val redacted = CameraIdentity(make = "Fujifilm", model = "X-T5", serial = "ABC123").redactedForExport()
            assertNull(redacted.serial)
            assertEquals("Fujifilm", redacted.make)
            assertEquals("X-T5", redacted.model)
        }

        @Test
        fun `coordinates are rounded to roughly a kilometre`() {
            val fix =
                GpsFix(
                    latitude = 51.507351,
                    longitude = -0.127758,
                    source = GpsSource.Exif.asWire(),
                )
            val redacted = fix.redactedForExport()
            assertTrue(abs(redacted.latitude - 51.51) < EPSILON, "got ${redacted.latitude}")
            assertTrue(abs(redacted.longitude - -0.13) < EPSILON, "got ${redacted.longitude}")
        }

        @Test
        fun `redaction keeps the datum, because the rounded value is still in it`() {
            val fix = GpsFix(39.9087, 116.3975, GpsSource.Manual.asWire(), GpsDatum.Gcj02.asWire())
            assertEquals(GpsDatum.Gcj02.asWire(), fix.redactedForExport().datum)
        }

        @Test
        fun `device and session identifiers are stripped from an exported asset`() {
            val exported =
                asset()
                    .copy(
                        importedByDevice = DeviceId("d-1"),
                        importedInSession = SessionId("s-1"),
                        camera = CameraIdentity("Fujifilm", "X-T5", "ABC123"),
                        gps = GpsFix(51.507351, -0.127758, GpsSource.Exif.asWire()),
                    ).redactedForExport()

            assertNull(exported.importedByDevice)
            assertNull(exported.importedInSession)
            assertNull(exported.camera?.serial)
            assertTrue(abs(exported.gps!!.latitude - 51.51) < EPSILON)
        }

        @Test
        fun `redacting leaves everything else untouched`() {
            // Stripping happens at the moment of export; the library's own copy is unchanged,
            // so nothing but the listed fields may differ.
            val original = asset().copy(rating = 4, isHidden = true)
            val exported = original.redactedForExport()
            assertEquals(original.id, exported.id)
            assertEquals(4, exported.rating)
            assertTrue(exported.isHidden)
        }

        @Test
        fun `wgs84 is the wire-absent default`() {
            assertTrue(GpsDatum.Wgs84.isWireAbsentDefault)
            assertFalse(GpsDatum.Gcj02.isWireAbsentDefault)
            assertEquals(GpsDatum.Wgs84.asWire(), GpsFix(0.0, 0.0, GpsSource.Exif.asWire()).datum)
        }
    }

    @Nested
    inner class ColourEncoding {
        @Test
        fun `hex round-trips`() {
            val colour = RgbColor(red = 18, green = 52, blue = 86)
            assertEquals("#123456", colour.toHex())
            assertEquals(colour, RgbColor.fromHex("#123456"))
            assertEquals(colour, RgbColor.fromHex("123456"))
        }

        @Test
        fun `the channel order is red, green, blue`() {
            // A swapped pair here would tint every placeholder in the library.
            assertEquals(RgbColor(255, 0, 0), RgbColor.fromHex("#ff0000"))
            assertEquals(RgbColor(0, 255, 0), RgbColor.fromHex("#00ff00"))
            assertEquals(RgbColor(0, 0, 255), RgbColor.fromHex("#0000ff"))
        }

        @Test
        fun `extremes survive the round trip`() {
            listOf(RgbColor(0, 0, 0), RgbColor(255, 255, 255)).forEach {
                assertEquals(it, RgbColor.fromHex(it.toHex()))
            }
        }

        @Test
        fun `a malformed value is null rather than a guess`() {
            assertNull(RgbColor.fromHex("#12345"))
            assertNull(RgbColor.fromHex("#gggggg"))
            assertNull(RgbColor.fromHex(""))
        }
    }

    @Nested
    inner class TimelineGeometry {
        @Test
        fun `a day bucket describes a contiguous run`() {
            val bucket = DayBucket(LocalDate.of(2026, 7, 3), startOffset = 40, count = 12)
            assertEquals(52, bucket.endOffsetExclusive)
            assertEquals("2026-07-03", bucket.key)
        }

        @Test
        fun `one bucket's end is the next one's start`() {
            val first = DayBucket(LocalDate.of(2026, 7, 3), startOffset = 0, count = 10)
            val second = DayBucket(LocalDate.of(2026, 7, 2), startOffset = first.endOffsetExclusive, count = 4)
            assertEquals(10, second.startOffset)
            assertEquals(14, second.endOffsetExclusive)
        }

        @Test
        fun `an empty day still has a well-formed run`() {
            val empty = DayBucket(LocalDate.of(2026, 7, 3), startOffset = 7, count = 0)
            assertEquals(7, empty.endOffsetExclusive)
        }

        @Test
        fun `dimensions derive their own ratio`() {
            assertEquals(4f / 3f, Dimensions(width = 4032, height = 3024).aspectRatio, RATIO_EPSILON)
        }
    }

    @Nested
    inner class LinkCapsEnforcement {
        @Test
        fun `an uncapped link accepts deposits`() {
            assertTrue(link().acceptsDepositsAt(T0))
        }

        @Test
        fun `a revoked link stops accepting`() {
            assertFalse(link().copy(revokedAt = T0).acceptsDepositsAt(T0))
        }

        @Test
        fun `an expired link stops accepting at its deadline`() {
            val expiring = link(LinkCaps(expiresAt = T0.plus(Duration.ofHours(1))))
            assertTrue(expiring.acceptsDepositsAt(T0))
            assertFalse(expiring.acceptsDepositsAt(T0.plus(Duration.ofHours(1))))
        }

        @Test
        fun `the file-count and byte caps both bind`() {
            val capped = link(LinkCaps(maxFileCount = 2, maxTotalBytes = 1000))
            assertTrue(capped.copy(filesReceived = 1, bytesReceived = 999).acceptsDepositsAt(T0))
            assertFalse(capped.copy(filesReceived = 2, bytesReceived = 0).acceptsDepositsAt(T0))
            assertFalse(capped.copy(filesReceived = 0, bytesReceived = 1000).acceptsDepositsAt(T0))
        }

        @Test
        fun `a share link is live until it is revoked or expires`() {
            val share =
                ShareLink(
                    id = ShareLinkId("sl-1"),
                    opaqueId = "0123456789abcdef0123456789abcdef",
                    scope = ShareScope.WholeAlbum(AlbumId("alb-1")),
                    createdAt = T0,
                    expiresAt = T0.plus(Duration.ofDays(7)),
                )
            assertTrue(share.isLiveAt(T0))
            assertFalse(share.isLiveAt(T0.plus(Duration.ofDays(7))))
            assertFalse(share.copy(revokedAt = T0).isLiveAt(T0))
        }

        @Test
        fun `a share link with no expiry never expires on its own`() {
            val share =
                ShareLink(
                    id = ShareLinkId("sl-2"),
                    opaqueId = "fedcba9876543210fedcba9876543210",
                    scope = ShareScope.SingleAsset(AssetId("a-1")),
                    createdAt = T0,
                )
            assertTrue(share.isLiveAt(T0.plus(Duration.ofDays(3650))))
        }

        private fun link(caps: LinkCaps = LinkCaps()) =
            UploadLink(
                id = UploadLinkId("ul-1"),
                opaqueId = "aaaabbbbccccddddeeeeffff00001111",
                caps = caps,
                createdAt = T0,
            )
    }

    @Nested
    inner class Aggregates {
        @Test
        fun `an album that is the default cannot be deleted`() {
            // Import always needs a home, so the user repoints before deleting.
            assertFalse(album(isDefault = true).isDeletable)
            assertTrue(album(isDefault = false).isDeletable)
        }

        @Test
        fun `a stack falls back to its primary when it has no cover`() {
            assertEquals(AssetId("primary"), stack(cover = null).displayAssetId)
            assertEquals(AssetId("cover"), stack(cover = AssetId("cover")).displayAssetId)
        }

        @Test
        fun `an import plan is ready only when it has work and no conflicts`() {
            val candidate = ImportCandidate("/dcim/a.jpg", 100, ImportDecision.Import(AlbumId("alb-1")))
            assertTrue(plan(added = listOf(candidate)).isReady)
            assertFalse(plan(added = emptyList()).isReady)
            assertFalse(plan(added = listOf(candidate), conflicts = listOf(candidate)).isReady)
        }

        @Test
        fun `a restore report needs review only for the cases the ladder defers`() {
            val divergent = RestoreConflict(AssetId("a"), RestoreConflictKind.Divergent.asWire(), null, null)
            val identical = RestoreConflict(AssetId("b"), RestoreConflictKind.Identical.asWire(), null, null)
            assertTrue(report(listOf(divergent)).needsReview)
            assertFalse(report(listOf(identical)).needsReview)
            assertFalse(report(emptyList()).needsReview)
        }

        @Test
        fun `only an absent asset is applied without asking`() {
            assertTrue(RestoreConflictKind.AbsentLocally.appliesOnCommit)
            listOf(
                RestoreConflictKind.Identical,
                RestoreConflictKind.LiveHeadNewer,
                RestoreConflictKind.Divergent,
            ).forEach { assertFalse(it.appliesOnCommit, "$it must not apply silently") }
        }

        private fun album(isDefault: Boolean) =
            Album(
                id = AlbumId("alb-1"),
                name = if (isDefault) "" else "Trip",
                createdAt = T0,
                modifiedAt = T0,
                isDefault = isDefault,
            )

        private fun stack(cover: AssetId?) =
            Stack(
                id = StackId("st-1"),
                stackType = StackType.Burst.asWire(),
                primaryAssetId = AssetId("primary"),
                coverAssetId = cover,
                createdAt = T0,
                modifiedAt = T0,
            )

        private fun plan(
            added: List<ImportCandidate>,
            conflicts: List<ImportCandidate> = emptyList(),
        ) = ImportPlan(
            importId = ImportId("imp-1"),
            added = added,
            skipped = emptyList(),
            conflicts = conflicts,
            totalSize = added.sumOf(ImportCandidate::sizeBytes),
        )

        private fun report(conflicts: List<RestoreConflict>) =
            RestoreReport(
                verified = 1,
                wouldAdd = emptyList(),
                identical = emptyList(),
                conflicts = conflicts,
            )
    }

    private fun asset() =
        Asset(
            id = AssetId("a-1"),
            assetType = AssetType.Photo.asWire(),
            captureTimestamp = T0,
            captureUtc = T0,
            captureTzSource = CaptureTzSource.OffsetExif.asWire(),
            importTimestamp = T0,
            contentHash = ContentHash("a".repeat(64)),
            dimensions = Dimensions(4032, 3024),
            duration = null,
            albumId = AlbumId("alb-1"),
            rating = 0,
            isHidden = false,
        )

    private companion object {
        val T0: Instant = Instant.parse("2026-01-01T00:00:00Z")
        const val EPSILON = 1e-9
        const val RATIO_EPSILON = 1e-6f
    }
}
