package com.justin13888.capsule

import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.labels.FfiLabelPort
import com.justin13888.capsule.screens.mock.MockEnvironment
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiWorkspace
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertSame

/** The two worlds the screens run on: the mock one, and the one over a real workspace. */
class ScreenPortsTest {
    @Test
    fun theMockWorldIsTheMockEnvironmentsPortsAndSubjects() {
        val environment = MockEnvironment(seed = 3)
        val ports = ScreenPorts.mock(environment)
        assertSame(environment.labels, ports.labels)
        assertSame(environment.transferEvents, ports.transferEvents)
        assertEquals(environment.sampleAsset, ports.sampleAsset)
        assertEquals(environment.sampleStack, ports.sampleStack)
        assertEquals(environment.sampleAlbum, ports.sampleAlbum)
    }

    @Test
    fun theRustBackedWorldLabelsTheWorkspacesDefaultAlbumAndHasNoEventsWithoutASession() =
        runBlocking {
            val root = Files.createTempDirectory("capsule-android-ports")
            val workspace =
                FfiWorkspace.create(
                    root.toString(),
                    "correct horse".toByteArray(),
                    FfiDeviceTier.NORMAL,
                    FfiClientBuild("capsule-android", "0.0.0"),
                )
            try {
                val album = workspace.ensureAlbum(workspace.defaultAlbumId(), "Camera Roll")
                val ports = ScreenPorts.rustBacked(workspace, session = null)
                assertIs<FfiLabelPort>(ports.labels)
                assertEquals(album, ports.sampleAlbum?.value)
                assertNull(ports.sampleAsset)
                assertNull(ports.sampleStack)
                assertEquals(listOf("Trips"), ports.labels.addLabel("Trips", LabelTarget.Album(ports.sampleAlbum!!)))
                assertEquals(emptyList(), ports.transferEvents.events().toList())
            } finally {
                workspace.close()
                root.toFile().deleteRecursively()
            }
        }
}
