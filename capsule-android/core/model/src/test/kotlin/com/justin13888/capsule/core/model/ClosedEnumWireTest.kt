package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.TestFactory

/** One closed enum, its codec, and the exact wire vocabulary the design pins for it. */
private class ClosedEnumCase(
    val name: String,
    val codec: WireCodec<out WireEnum>,
    val constants: List<WireEnum>,
    val expectedWire: List<String>,
)

/**
 * The wire contract for every closed enum in the module.
 *
 * The expected token lists are written out longhand rather than derived from the constants,
 * which is the entire point: deriving them would make the test agree with whatever the code
 * currently says, including after a rename that silently changed a value two implementations
 * have to agree on byte for byte.
 */
class ClosedEnumWireTest {
    private val cases =
        listOf(
            ClosedEnumCase("AssetType", AssetType, AssetType.entries, listOf("photo", "video")),
            ClosedEnumCase("MediaKind", MediaKind, MediaKind.entries, listOf("image", "video")),
            ClosedEnumCase(
                "ContentType",
                ContentType,
                ContentType.entries,
                listOf(
                    "image/jpeg",
                    "image/png",
                    "image/webp",
                    "image/gif",
                    "image/tiff",
                    "image/heic",
                    "image/avif",
                    "image/jxl",
                    "image/x-adobe-dng",
                    "video/mp4",
                    "video/quicktime",
                    "video/x-matroska",
                    "video/webm",
                ),
            ),
            ClosedEnumCase(
                "CaptureTzSource",
                CaptureTzSource,
                CaptureTzSource.entries,
                listOf("offset_exif", "gps_lookup", "floating"),
            ),
            ClosedEnumCase("CullFlag", CullFlag, CullFlag.entries, listOf("pick", "neutral", "reject")),
            ClosedEnumCase("ImportMode", ImportMode, ImportMode.entries, listOf("copy", "move")),
            ClosedEnumCase("GpsSource", GpsSource, GpsSource.entries, listOf("exif", "manual", "inferred")),
            ClosedEnumCase("GpsDatum", GpsDatum, GpsDatum.entries, listOf("wgs84", "gcj02")),
            ClosedEnumCase("AlbumKind", AlbumKind, AlbumKind.entries, listOf("container", "view")),
            ClosedEnumCase(
                "AlbumCapability",
                AlbumCapability,
                AlbumCapability.entries,
                listOf("read", "write", "admin"),
            ),
            ClosedEnumCase(
                "StackType",
                StackType,
                StackType.entries,
                listOf(
                    "raw_jpeg",
                    "burst",
                    "live_photo",
                    "portrait",
                    "smart_selection",
                    "hdr_bracket",
                    "focus_stack",
                    "pixel_shift",
                    "panorama",
                    "proxy",
                    "chaptered",
                    "dual_audio",
                    "custom",
                ),
            ),
            ClosedEnumCase("StackRole", StackRole, StackRole.entries, listOf("primary", "member", "proxy")),
            ClosedEnumCase(
                "StackMemberRole",
                StackMemberRole,
                StackMemberRole.entries,
                listOf(
                    "primary",
                    "raw",
                    "video",
                    "audio",
                    "depth_map",
                    "processed",
                    "source",
                    "alternate",
                    "sidecar",
                    "proxy",
                    "master",
                ),
            ),
            ClosedEnumCase(
                "DetectionMethod",
                DetectionMethod,
                DetectionMethod.entries,
                listOf("filename_stem", "content_identifier", "timecode", "manual"),
            ),
            ClosedEnumCase(
                "LifecycleAction",
                LifecycleAction,
                LifecycleAction.entries,
                listOf(
                    "create",
                    "replace",
                    "delete",
                    "metadata-update",
                    "derivative-add",
                    "derivative-replace",
                    "trash-restore",
                ),
            ),
            ClosedEnumCase(
                "RepresentationTier",
                RepresentationTier,
                RepresentationTier.entries,
                listOf("lqip", "thumbnail", "preview", "original"),
            ),
            ClosedEnumCase("UploadTier", UploadTier, UploadTier.entries, listOf("t0", "t1", "t2")),
            ClosedEnumCase("UploadPolicy", UploadPolicy, UploadPolicy.entries, listOf("full", "staged")),
            ClosedEnumCase(
                "ConnectionClass",
                ConnectionClass,
                ConnectionClass.entries,
                listOf("unmetered", "metered", "constrained", "adverse", "offline"),
            ),
            ClosedEnumCase(
                "SyncScope",
                SyncScope,
                SyncScope.entries,
                listOf("metadata_only", "metadata_thumbnails", "metadata_thumbnails_original"),
            ),
            ClosedEnumCase(
                "QuotaState",
                QuotaState,
                QuotaState.entries,
                listOf("ok", "soft_warning", "hard_exceeded", "grace_expired", "suspended"),
            ),
            ClosedEnumCase(
                "PlatformTag",
                PlatformTag,
                PlatformTag.entries,
                listOf("ios", "android", "macos", "windows", "linux"),
            ),
            ClosedEnumCase(
                "SourceKind",
                SourceKind,
                SourceKind.entries,
                listOf("camera_roll", "screenshots", "app_collection", "folder", "watched_dir", "removable_volume"),
            ),
            ClosedEnumCase(
                "ModerationEventKind",
                ModerationEventKind,
                ModerationEventKind.entries,
                listOf("takedown", "takedown_lifted", "legal_hold", "suspended", "unsuspended"),
            ),
            ClosedEnumCase(
                "RestoreMode",
                RestoreMode,
                RestoreMode.entries,
                listOf("preview", "dry_run", "commit"),
            ),
            ClosedEnumCase(
                "QueryField",
                QueryField,
                QueryField.entries,
                listOf(
                    "capture_timestamp",
                    "import_timestamp",
                    "content_type",
                    "media_kind",
                    "gps.datum",
                    "rating",
                    "dimensions.width",
                    "dimensions.height",
                    "cull",
                    "hidden",
                    "tags_user",
                    "tags_ai",
                    "stack_type",
                    "people_cluster",
                    "album_id",
                    "gps",
                    "camera_id",
                ),
            ),
            ClosedEnumCase(
                "Operator",
                Operator,
                Operator.entries,
                listOf(
                    "before",
                    "after",
                    "in_range",
                    "eq",
                    "in",
                    "gte",
                    "lte",
                    "is",
                    "contains",
                    "contains_any",
                    "contains_all",
                    "exists",
                ),
            ),
            ClosedEnumCase(
                "ImportFileState",
                ImportFileState,
                ImportFileState.entries,
                listOf("queued", "processing", "encrypting", "uploading", "done", "failed"),
            ),
        )

    @TestFactory
    fun `every constant round-trips through its wire token`(): List<DynamicTest> =
        cases.map { case ->
            DynamicTest.dynamicTest(case.name) {
                case.constants.forEach { constant ->
                    val parsed = case.codec.fromWire(constant.wire)
                    assertTrue(parsed.isKnown, "${case.name}: '${constant.wire}' did not resolve")
                    assertSame(
                        constant,
                        parsed.knownOrNull,
                        "${case.name}: '${constant.wire}' resolved to the wrong constant",
                    )
                    assertEquals(constant.wire, parsed.raw, "${case.name}: raw token changed on the way through")
                }
            }
        }

    @TestFactory
    fun `wire vocabulary matches the pinned value set exactly`(): List<DynamicTest> =
        cases.map { case ->
            DynamicTest.dynamicTest(case.name) {
                assertEquals(case.expectedWire, case.constants.map(WireEnum::wire), case.name)
            }
        }

    @TestFactory
    fun `no two constants share a wire token`(): List<DynamicTest> =
        cases.map { case ->
            DynamicTest.dynamicTest(case.name) {
                val tokens = case.constants.map(WireEnum::wire)
                assertEquals(tokens.size, tokens.toSet().size, "${case.name}: duplicate wire token")
            }
        }

    @TestFactory
    fun `an unknown token is preserved rather than dropped`(): List<DynamicTest> =
        cases.map { case ->
            DynamicTest.dynamicTest(case.name) {
                val alien = "definitely_not_a_${case.name.lowercase()}_value"
                val parsed = case.codec.fromWire(alien)
                assertEquals(Wire.Unknown(alien), parsed, "${case.name}: unknown token was not preserved")
                assertEquals(alien, parsed.raw, "${case.name}: unknown token lost its raw form")
                assertNull(parsed.knownOrNull, "${case.name}: unknown token resolved to a constant")
                assertNull(case.codec.fromWireOrNull(alien), "${case.name}: fromWireOrNull invented a constant")
            }
        }

    @Test
    fun `an empty token is unknown, not a default`() {
        // A missing value must never silently become the first constant of the set; the
        // wire-absent defaults are applied by the decoder, not by the codec.
        assertEquals(Wire.Unknown(""), CullFlag.fromWire(""))
    }

    @Test
    fun `case is significant`() {
        // The canonical form is lowercase snake or kebab case; a case-folded match here would
        // let two implementations disagree on bytes that a signature covers.
        assertNull(StackType.fromWireOrNull("RAW_JPEG"))
        assertNull(LifecycleAction.fromWireOrNull("Metadata-Update"))
    }

    @Test
    fun `the lifecycle action set is kebab-cased, not snake-cased`() {
        // The manifest spells these with hyphens and its bytes are what the signatures cover,
        // so normalising them to snake_case here would break cross-peer verification.
        assertNull(LifecycleAction.fromWireOrNull("metadata_update"))
        assertSame(LifecycleAction.MetadataUpdate, LifecycleAction.fromWireOrNull("metadata-update"))
    }

    @Test
    fun `asWire lifts a constant and reports it as known`() {
        val lifted = StackType.LivePhoto.asWire()
        assertTrue(lifted.isKnown)
        assertEquals("live_photo", lifted.raw)
        assertSame(StackType.LivePhoto, lifted.knownOrNull)
    }

    @Test
    fun `fromWireOrThrow refuses an unknown token`() {
        val thrown = runCatching { StackType.fromWireOrThrow("future-stack-type") }.exceptionOrNull()
        assertTrue(thrown is IllegalArgumentException, "expected a structural rejection, got $thrown")
    }
}
