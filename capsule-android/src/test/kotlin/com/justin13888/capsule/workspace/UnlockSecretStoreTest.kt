package com.justin13888.capsule.workspace

import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/** The unlock secret: drawn once, sealed before it is stored, and never replaced. */
class UnlockSecretStoreTest {
    private val dir = Files.createTempDirectory("capsule-android-unlock").toFile()

    @AfterTest
    fun cleanUp() {
        dir.deleteRecursively()
    }

    @Test
    fun theFirstLoadDrawsAndSealsASecretThatEveryLaterLoadReturns() {
        val random = CountingRandom()
        val sealer = FakeSealer()
        val store = unlockStore(dir, sealer, random)
        assertFalse(store.exists())
        val first = store.loadOrCreate()
        assertEquals(UnlockSecretStore.SECRET_BYTES, first.size)
        assertTrue(store.exists())
        assertContentEquals(first, store.loadOrCreate())
        assertContentEquals(first, unlockStore(dir, sealer, random).load())
        assertEquals(1, random.draws)
        assertEquals(1, sealer.sealed)
    }

    @Test
    fun theSecretIsStoredSealedNeverInTheClear() {
        val store = unlockStore(dir)
        val secret = store.loadOrCreate()
        val stored = File(dir, "unlock.sealed").readBytes()
        assertFalse(stored.contentEquals(secret))
        assertFalse(File(dir, "unlock.sealed.partial").exists())
    }

    @Test
    fun aSecretThatDoesNotOpenIsReportedAndLeftInPlace() {
        val random = CountingRandom()
        unlockStore(dir, random = random).loadOrCreate()
        val file = File(dir, "unlock.sealed")
        val tampered = byteArrayOf(0) + file.readBytes().drop(1)
        file.writeBytes(tampered)
        assertFailsWith<UnlockSecretUnavailableException> { unlockStore(dir, random = random).loadOrCreate() }
        assertContentEquals(tampered, file.readBytes())
        assertEquals(1, random.draws)
    }
}
