package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

/** The mock world labels exactly its sample asset, stack and album, the same ones on every build. */
class MockEnvironmentTest {
    @Test
    fun theSampleSubjectsTakeLabelsAndNothingElseDoes() =
        runBlocking<Unit> {
            val environment = MockEnvironment()
            assertEquals(emptyList(), environment.labels.regions(environment.sampleAsset))
            assertEquals(listOf("x"), environment.labels.addLabel("x", LabelTarget.Stack(environment.sampleStack)))
            assertEquals(listOf("y"), environment.labels.addLabel("y", LabelTarget.Album(environment.sampleAlbum)))
            assertFailsWith<LabelTargetNotFound> { environment.labels.addLabel("z", LabelTarget.Album(AlbumId("elsewhere"))) }
        }

    @Test
    fun twoEnvironmentsBuiltAlikeAreTheSameWorld() {
        val first = MockEnvironment(seed = 9)
        val second = MockEnvironment(seed = 9)
        assertEquals(first.sampleAsset, second.sampleAsset)
        assertEquals(first.sampleStack, second.sampleStack)
        assertEquals(first.sampleAlbum, second.sampleAlbum)
        assertEquals((first.transferEvents as MockTransferEventFeed).script, (second.transferEvents as MockTransferEventFeed).script)
    }
}
