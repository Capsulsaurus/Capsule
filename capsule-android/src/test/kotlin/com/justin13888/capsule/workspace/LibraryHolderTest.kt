package com.justin13888.capsule.workspace

import com.justin13888.capsule.hardware.SoftwareP256Signer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiHardwareSigner
import uniffi.capsule_sdk.FfiHardwareSignerException
import java.io.File
import java.io.IOException
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
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

    private val choice = FileSoftwareKeyChoice(File(dir, "software-keys"))

    /** Builds each attempt's lifecycle over [element], or over software keys once the choice holds them. */
    private fun holder(
        root: File = File(dir, "library"),
        secrets: UnlockSecretStore = unlockStore(dir),
        element: FfiHardwareSigner? = null,
        softwareKeys: SoftwareKeyChoice = choice,
    ) = LibraryHolder(
        lifecycle = { software ->
            LibraryLifecycle(
                root,
                secrets,
                FfiDeviceTier.LOW_RAM,
                FfiClientBuild("capsule-android", "0.0.0"),
                "Library",
                signer = if (software) null else element,
            )
        },
        softwareKeys = softwareKeys,
        scope = scope,
        dispatcher = Dispatchers.Unconfined,
    )

    /** A keystore that refuses every enrollment, counting each one it was asked for. */
    private class RefusingElement(
        private val element: FfiHardwareSigner = SoftwareP256Signer(ByteArray(32) { 7 }),
    ) : FfiHardwareSigner by element {
        var enrollments = 0

        override fun enroll(keyAlias: String): ByteArray {
            enrollments++
            throw FfiHardwareSignerException.Unavailable("keystore refused")
        }
    }

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

    @Test
    fun aRefusedEnrollmentOffersSoftwareKeysAndARetryAsksTheKeystoreAgain() {
        val element = RefusingElement()
        val holder = holder(element = element)
        val refused = holder.state.value
        assertIs<LibraryState.Failed>(refused)
        assertTrue(refused.hardwareRefused)
        assertFalse(refused.locked)

        holder.retry()
        val again = holder.state.value
        assertIs<LibraryState.Failed>(again)
        assertTrue(again.hardwareRefused)
        assertEquals(2, element.enrollments)
        // Retrying is not choosing: nothing was remembered and nothing laid out.
        assertFalse(choice.chosen())
        assertFalse(File(dir, "library/${LibraryLifecycle.LIBRARY_DIR}").exists())
    }

    @Test
    fun continuingWithSoftwareKeysCreatesTheLibraryAndALaterLaunchKeepsThem() {
        val element = RefusingElement()
        val holder = holder(element = element)
        holder.continueWithSoftwareKeys()
        val ready = holder.state.value
        assertIs<LibraryState.Ready>(ready)
        assertTrue(choice.chosen())
        val user = ready.library.workspace.userId()
        (ready.library.workspace as AutoCloseable).close()

        // The next launch opens on software keys without asking the keystore again.
        val relaunch = holder(element = element).state.value
        assertIs<LibraryState.Ready>(relaunch)
        assertEquals(user, relaunch.library.workspace.userId())
        assertEquals(1, element.enrollments)
        (relaunch.library.workspace as AutoCloseable).close()
    }

    @Test
    fun aFailureThatIsNotARefusalOffersNoSoftwareKeys() {
        val root = File(dir, "library").apply { writeBytes(byteArrayOf(0)) }
        val holder = holder(root = root)
        val failed = holder.state.value
        assertIs<LibraryState.Failed>(failed)
        assertFalse(failed.hardwareRefused)

        holder.continueWithSoftwareKeys()
        assertIs<LibraryState.Failed>(holder.state.value)
        assertFalse(choice.chosen())
    }

    @Test
    fun aChoiceThatCannotBeRecordedLeavesTheRefusalOnScreen() {
        val unrecordable =
            object : SoftwareKeyChoice {
                override fun chosen() = false

                override fun choose(): Unit = throw IOException("read-only")
            }
        val holder = holder(element = RefusingElement(), softwareKeys = unrecordable)
        holder.continueWithSoftwareKeys()
        val state = holder.state.value
        assertIs<LibraryState.Failed>(state)
        assertTrue(state.hardwareRefused)
        assertFalse(File(dir, "library/${LibraryLifecycle.LIBRARY_DIR}").exists())
    }
}
