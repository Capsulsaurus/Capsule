package com.justin13888.capsule.feature.people

import android.content.Context
import androidx.annotation.StringRes
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.Dimensions
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.ModelId
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.ModelVersion
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.asWire
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * One generated library shared by every test in this module.
 *
 * Generation is the expensive half of the mock and it is pure, so regenerating per test would
 * buy nothing but a slower suite. The mutable half is per-test regardless: each
 * [repositories] call builds a fresh state over the same immutable fixture, so no test can
 * observe another's writes.
 */
object PeopleFixtures {
    /** Structure is what these tests assert, so the small fixture is the right one. */
    val SPEC: LibrarySpec = LibrarySpec.SMALL

    /** The model slot the fixture's current outputs came from. */
    val CURRENT_SLOT: ModelSlot = ModelSlot(ModelId("facenet-s"), ModelVersion("2026.02"))

    /** A slot that has since been superseded. */
    val SUPERSEDED_SLOT: ModelSlot = ModelSlot(ModelId("facenet-s"), ModelVersion("2025.05"))

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    private val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over a fresh mutable state. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario))

    /** A hub view model wired to [repositories]. */
    fun hubViewModel(repositories: MockRepositories): PeopleViewModel =
        PeopleViewModel(
            people = repositories.people,
            assets = repositories.assets,
            timeline = repositories.timeline,
            metadata = repositories.metadata,
        )

    /** A person view model wired to [repositories]. */
    fun personViewModel(
        repositories: MockRepositories,
        personId: String,
    ): PersonDetailViewModel =
        PersonDetailViewModel(
            personId = personId,
            people = repositories.people,
            metadata = repositories.metadata,
        )
}

/** The localized value of [id], for asserting against text the screen actually renders. */
fun localized(
    @StringRes id: Int,
): String = ApplicationProvider.getApplicationContext<Context>().getString(id)

/** A minimal asset, with everything the smart-album engine reads exposed as a parameter. */
@Suppress("LongParameterList")
fun testAsset(
    id: String,
    capturedAt: Instant = Instant.parse("2026-03-04T10:00:00Z"),
    rating: Int = 0,
    located: Boolean = false,
    assetType: AssetType = AssetType.Photo,
): Asset =
    Asset(
        id = AssetId(id),
        assetType = assetType.asWire(),
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
        gps = if (located) testFix() else null,
    )

/** A fix somewhere. Coordinates are irrelevant to a trip; presence and timing are. */
fun testFix(): GpsFix = GpsFix(latitude = 49.28, longitude = -123.12, source = GpsSource.Exif.asWire())

/** Model suggestions on one asset, on the slot the library currently runs. */
fun freshAiTags(vararg words: String): AssetTags =
    AssetTags(
        ai =
            words.mapIndexed { index, word ->
                AiTag(word, AddId(DeviceId("device-1"), index.toLong()), PeopleFixtures.CURRENT_SLOT)
            },
    )

/** Model suggestions from a model version that has since been replaced. */
fun staleAiTags(vararg words: String): AssetTags =
    AssetTags(
        ai =
            words.mapIndexed { index, word ->
                AiTag(
                    word,
                    AddId(DeviceId("device-1"), index.toLong()),
                    PeopleFixtures.SUPERSEDED_SLOT,
                    isStale = true,
                )
            },
    )

/** Tags the user wrote themselves, which no automatic album is allowed to read. */
fun userTags(vararg words: String): AssetTags =
    AssetTags(user = words.mapIndexed { index, word -> UserTag(word, AddId(DeviceId("device-1"), index.toLong())) })
