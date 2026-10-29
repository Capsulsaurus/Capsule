package com.justin13888.capsule.workspace

import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiWorkspace
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The library's lifecycle over a real `FfiWorkspace` (JNA loads the host `libcapsule_sdk`):
 * created on the first launch, reopened as the same account on every launch after, and
 * never recreated over a library whose secret is gone.
 */
class LibraryLifecycleTest {
    private val dir = Files.createTempDirectory("capsule-android-library").toFile()
    private val root = File(dir, "library")
    private val sealer = FakeSealer()
    private val opened = mutableListOf<OpenedLibrary>()

    @AfterTest
    fun cleanUp() {
        opened.forEach { (it.workspace as? AutoCloseable)?.close() }
        dir.deleteRecursively()
    }

    private fun lifecycle(secrets: UnlockSecretStore = unlockStore(dir, sealer)) =
        LibraryLifecycle(root, secrets, FfiDeviceTier.LOW_RAM, FfiClientBuild("capsule-android", "0.0.0"), "Library")

    private fun open(lifecycle: LibraryLifecycle = lifecycle()): OpenedLibrary = lifecycle.open().also { opened += it }

    @Test
    fun theFirstLaunchCreatesTheLibraryWithItsDefaultAlbumAndNoAssets() {
        val library = open()
        assertTrue(File(root, LibraryLifecycle.LIBRARY_DIR).isDirectory)
        val defaultAlbum = library.workspace.defaultAlbumId()
        assertEquals(listOf(defaultAlbum), library.workspace.albums().map { it.albumId })
        assertNull(library.sampleAsset)
    }

    @Test
    fun aRelaunchOpensTheSameAccountAndOffersItsFirstAsset() {
        val first = open()
        val user = first.workspace.userId()
        val asset = first.workspace.sealAsset(first.workspace.defaultAlbumId(), "IMG_0001.JPG", "photo".toByteArray())
        (first.workspace as AutoCloseable).close()
        opened.clear()

        val second = open()
        assertEquals(user, second.workspace.userId())
        assertEquals(asset, second.sampleAsset?.value)
        assertEquals(1, second.workspace.albums().size)
    }

    @Test
    fun aRelaunchAfterTheProcessWasKilledOpensDespiteTheLockItLeft() {
        // The core reads liveness from /proc, as on a device; a host without it keeps every lock.
        val pidMax = File("/proc/sys/kernel/pid_max").takeIf { it.isFile } ?: return
        val first = open()
        val user = first.workspace.userId()
        val lock = File(root, "${LibraryLifecycle.LIBRARY_DIR}/lock")
        val held = lock.readText()
        (first.workspace as AutoCloseable).close()
        opened.clear()
        // What a killed process leaves: its lock, naming a process that no longer runs.
        // Every pid is below pid_max, so none runs under it.
        val dead = pidMax.readText().trim()
        lock.writeText(held.replace(Regex("\"pid\":\\d+"), "\"pid\":$dead"))

        assertEquals(user, open().workspace.userId())
    }

    @Test
    fun aFirstLaunchKilledWhileTheLibraryWasLaidOutIsCreatedAgain() {
        // The skeleton `init_library` leaves when killed before `version.cbor` is written.
        File(root, "${LibraryLifecycle.LIBRARY_DIR}/trash").mkdirs()
        File(root, "media").mkdirs()
        File(root, "index/thumbnails").mkdirs()

        val library = open()
        assertTrue(File(root, "${LibraryLifecycle.LIBRARY_DIR}/version.cbor").isFile)
        assertEquals(listOf(library.workspace.defaultAlbumId()), library.workspace.albums().map { it.albumId })
    }

    @Test
    fun anUnfinishedLibraryHoldingMediaIsNeverCleared() {
        val media = File(root, "media/IMG_0001.JPG")
        media.parentFile.mkdirs()
        media.writeBytes("photo".toByteArray())
        File(root, LibraryLifecycle.LIBRARY_DIR).mkdirs()

        assertFails { lifecycle().open() }
        assertTrue(media.readBytes().contentEquals("photo".toByteArray()))
    }

    @Test
    fun aLibraryWhoseSecretIsGoneIsRefusedAndLeftAsItWas() {
        val secrets = unlockStore(dir, sealer)
        val first = open(lifecycle(secrets))
        val user = first.workspace.userId()
        (first.workspace as AutoCloseable).close()
        opened.clear()
        val secret = secrets.load()
        val account = File(root, "${LibraryLifecycle.LIBRARY_DIR}/account.cbor")
        val before = account.readBytes()
        File(dir, "unlock.sealed").delete()

        assertFailsWith<LibraryLockedException> { lifecycle().open() }
        // No new secret was drawn and no new library made over the old one.
        assertTrue(!File(dir, "unlock.sealed").exists())
        assertTrue(before.contentEquals(account.readBytes()))
        FfiWorkspace.open(root.path, secret, FfiDeviceTier.LOW_RAM, FfiClientBuild("capsule-android", "0.0.0")).use {
            assertEquals(user, it.userId())
        }
    }

    @Test
    fun aLibraryWhoseSecretNoLongerOpensIsRefused() {
        open().also { (it.workspace as AutoCloseable).close() }
        opened.clear()
        val otherDevice =
            unlockStore(
                dir,
                object : SecretSealer by sealer {
                    override fun open(sealed: ByteArray): ByteArray = error("keystore key invalidated")
                },
            )
        val failure = assertFailsWith<LibraryLockedException> { lifecycle(otherDevice).open() }
        assertTrue(failure.cause is UnlockSecretUnavailableException)
    }
}
