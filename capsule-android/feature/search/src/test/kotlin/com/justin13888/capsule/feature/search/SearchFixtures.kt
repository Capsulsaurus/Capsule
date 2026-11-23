package com.justin13888.capsule.feature.search

import android.content.Context
import androidx.annotation.StringRes
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.CameraIdentity
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.Dimensions
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * One generated library shared by every test in this module.
 *
 * Generation is the expensive half of the mock and it is pure, so regenerating per test
 * would buy nothing but a slower suite. The mutable half is per-test regardless: each
 * [repositories] call builds a fresh state over the same immutable fixture, so no test can
 * observe another's writes.
 */
object SearchFixtures {
    /** Structure is what these tests assert, so the small fixture is the right one. */
    val SPEC: LibrarySpec = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    private val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over a fresh mutable state. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario))

    /** A view model wired to [repositories]. */
    fun viewModel(repositories: MockRepositories): SearchViewModel =
        SearchViewModel(
            search = repositories.search,
            timeline = repositories.timeline,
            metadata = repositories.metadata,
            people = repositories.people,
            albums = repositories.albums,
        )
}

/** The localized value of [id], for asserting against text the screen actually renders. */
fun localized(
    @StringRes id: Int,
): String = ApplicationProvider.getApplicationContext<Context>().getString(id)

/**
 * A minimal asset, with everything the facet builder reads exposed as a parameter.
 *
 * Hand-built rather than drawn from the fixture because these cases are about *coverage of
 * the facet space* — an asset with no camera, an asset with an unknown cull token — and the
 * seeded library deliberately does not contain every combination at once.
 */
@Suppress("LongParameterList")
fun testAsset(
    id: String,
    capturedAt: Instant = Instant.parse("2026-03-04T10:00:00Z"),
    assetType: Wire<AssetType> = AssetType.Photo.asWire(),
    rating: Int = 0,
    cull: Wire<CullFlag> = CullFlag.Neutral.asWire(),
    camera: CameraIdentity? = CameraIdentity(make = "Fujifilm", model = "X-T5", serial = null),
    gps: GpsFix? = null,
): Asset =
    Asset(
        id = AssetId(id),
        assetType = assetType,
        captureTimestamp = capturedAt,
        captureUtc = capturedAt,
        captureTzSource = null,
        importTimestamp = capturedAt,
        contentHash = ContentHash(id),
        dimensions = Dimensions(width = 3000, height = 2000),
        duration = null,
        albumId = null,
        rating = rating,
        isHidden = false,
        cull = cull,
        camera = camera,
        gps = gps,
    )

/** A fix somewhere, for the place facet. Coordinates are irrelevant; presence is the facet. */
fun testFix(): GpsFix = GpsFix(latitude = 49.28, longitude = -123.12, source = GpsSource.Exif.asWire())

/** A candidate carrying exactly [points], for testing the facet algebra without a library. */
fun candidateOf(
    id: String,
    vararg points: FacetPoint,
): SearchCandidate =
    SearchCandidate(
        item = SearchIndexing.itemOf(testAsset(id)),
        points = points.toList().toImmutableList(),
    )

/** A point in [kind] labelled by its own id, which is all the algebra tests need. */
fun pointOf(
    kind: FacetKind,
    id: String,
): FacetPoint = FacetPoint(FacetRef(kind, id), FacetLabel.Data(id))

/** A state with nothing in it, for screens that are asserted one field at a time. */
fun blankState(): SearchUiState = SearchUiState(phase = SearchPhase.Entry, recents = persistentListOf())
