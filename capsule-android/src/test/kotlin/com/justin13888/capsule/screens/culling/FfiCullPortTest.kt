package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiCullFlag
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiStackMember
import uniffi.capsule_sdk.FfiWorkspace
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.nio.file.Files
import java.util.UUID
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The cull port over a real `FfiWorkspace` (JNA loads the host `libcapsule_sdk`, as the
 * `:core` smoke does): the pass is every live asset with its flag and stack membership,
 * flags are written, and the sweep trashes exactly the assets it is given.
 */
class FfiCullPortTest {
    private val root = Files.createTempDirectory("capsule-android-culling")
    private val workspace =
        FfiWorkspace.create(
            root.toString(),
            "correct horse".toByteArray(),
            FfiDeviceTier.NORMAL,
            FfiClientBuild("capsule-android", "0.0.0"),
        )
    private val album = workspace.ensureAlbum(workspace.defaultAlbumId(), "Camera Roll")
    private val assets = (1..3).map { AssetId(workspace.sealAsset(album, "IMG_000$it.JPG", "photo $it".toByteArray())) }
    private val port = FfiCullPort(workspace, Dispatchers.Unconfined)

    @AfterTest
    fun cleanUp() {
        workspace.close()
        root.toFile().deleteRecursively()
    }

    @Test
    fun thePassIsEveryLiveAssetWithItsFlagInIdOrder() =
        runBlocking {
            port.setCull(assets[1], CullFlag.PICK)
            port.setCull(assets[2], CullFlag.REJECT)

            val pass = port.pass()

            assertEquals(assets.sortedBy { it.value }, pass.map { it.id })
            assertEquals(
                mapOf(assets[0] to CullFlag.NEUTRAL, assets[1] to CullFlag.PICK, assets[2] to CullFlag.REJECT),
                pass.associate { it.id to it.flag },
            )
            assertEquals(FfiCullFlag.REJECT, workspace.cullFlag(assets[2].value))
            assertTrue(pass.all { it.stack == null }, "an unstacked library reads as lone assets")
        }

    @Test
    fun aTwoMemberStackReadsAsOneGroup() =
        runBlocking {
            // The FFI has no stack-editing verb, so the membership read is the one call this
            // test supplies; the flags, the ids and the pass around it come from the workspace.
            val stack = StackId(UUID.randomUUID().toString())
            val (cover, member) = assets.sortedBy { it.value }.take(2)
            val stacked =
                object : FfiWorkspaceInterface by workspace {
                    override fun stackMemberships(): List<FfiStackMember> =
                        listOf(
                            FfiStackMember(member.value, stack.value, isCover = false, memberIndex = 1u),
                            FfiStackMember(cover.value, stack.value, isCover = true, memberIndex = 0u),
                        )
                }

            val pass = FfiCullPort(stacked, Dispatchers.Unconfined).pass()

            assertEquals(StackMembership(stack, isCover = true, memberIndex = 0), pass.single { it.id == cover }.stack)
            assertEquals(StackMembership(stack, isCover = false, memberIndex = 1), pass.single { it.id == member }.stack)
            val groups = CullingPass(pass).groups
            assertEquals(2, groups.size, "the stack and the lone asset")
            assertEquals(listOf(cover, member), groups.single { it.stack == stack }.members.map { it.id })
            assertNull(pass.single { it.id !in setOf(cover, member) }.stack)
        }

    @Test
    fun theSweepTrashesExactlyTheAssetsItIsGiven() =
        runBlocking {
            port.setCull(assets[0], CullFlag.REJECT)
            port.setCull(assets[2], CullFlag.REJECT)

            assertEquals(listOf(assets[0]), port.moveToTrash(listOf(assets[0])))

            assertEquals(listOf(assets[1], assets[2]).sortedBy { it.value }, port.pass().map { it.id })
            assertEquals(listOf(assets[0].value), workspace.trash().map { it.assetId })
        }

    @Test
    fun aSweepThatCannotMoveAnAssetSaysWhatItMoved() =
        runBlocking {
            val missing = AssetId("not-a-uuid")

            val stopped = assertFailsWith<SweepIncomplete> { port.moveToTrash(listOf(assets[0], missing, assets[1])) }

            assertEquals(listOf(assets[0]), stopped.moved)
            assertTrue(port.pass().none { it.id == assets[0] })
        }

    @Test
    fun anUnknownStackFlagsNothingAndAFailedWriteIsAnError() =
        runBlocking<Unit> {
            assertEquals(emptyList(), port.flagStack(StackId(UUID.randomUUID().toString()), CullFlag.PICK))
            assertFailsWith<IllegalStateException> { port.setCull(AssetId("not-a-uuid"), CullFlag.PICK) }
        }
}
