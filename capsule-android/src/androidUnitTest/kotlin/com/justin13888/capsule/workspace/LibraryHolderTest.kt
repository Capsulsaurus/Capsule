package com.justin13888.capsule.workspace

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue

/** The state the screens wait on while the library opens, and what a failure leaves. */
class LibraryHolderTest {
    private val dir = Files.createTempDirectory("capsule-android-holder").toFile()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)

    @AfterTest
    fun cleanUp() {
        scope.cancel()
        dir.deleteRecursively()
    }

    private fun holder(
        root: File = File(dir, "library"),
        secrets: UnlockSecretStore = unlockStore(dir),
    ) = LibraryHolder(
        LibraryLifecycle(root, secrets, FfiDeviceTier.LOW_RAM, FfiClientBuild("capsule-android", "0.0.0"), "Library"),
        scope,
        Dispatchers.Unconfined,
    )

    @Test
    fun anOpenedLibraryIsReady() {
        val state = holder().state.value
        assertIs<LibraryState.Ready>(state)
        (state.library.workspace as AutoCloseable).close()
    }

    @Test
    fun aLibraryWithoutItsSecretIsLockedAndARetryDoesNotUnlockIt() {
        val first = holder().state.value
        assertIs<LibraryState.Ready>(first)
        (first.library.workspace as AutoCloseable).close()
        File(dir, "unlock.sealed").delete()

        val holder = holder()
        val locked = holder.state.value
        assertIs<LibraryState.Failed>(locked)
        assertTrue(locked.locked)
        holder.retry()
        assertIs<LibraryState.Failed>(holder.state.value)
    }

    @Test
    fun aFailureThatIsNotALockCanBeRetried() {
        // A root that is a file: the core cannot make a library there.
        val root = File(dir, "library").apply { writeBytes(byteArrayOf(0)) }
        val holder = holder(root = root)
        val failed = holder.state.value
        assertIs<LibraryState.Failed>(failed)
        assertEquals(false, failed.locked)

        root.delete()
        holder.retry()
        val ready = holder.state.value
        assertIs<LibraryState.Ready>(ready)
        (ready.library.workspace as AutoCloseable).close()
    }
}
