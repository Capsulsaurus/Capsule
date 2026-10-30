package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.capsule_sdk.FfiCullFlag
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.util.logging.Level
import java.util.logging.Logger

/**
 * [CullPort] over the workspace's cull verbs (`FfiWorkspace::assets_by_cull`, `set_cull`,
 * `flag_stack`) and its trash (`delete_asset`).
 *
 * The pass is every live asset of the library, read as the three flag sets and ordered by
 * id, which for UUIDv7 ids is the order they were created. The FFI lists no stack
 * membership yet, so every asset arrives as a lone one; [CullingPass] groups whatever
 * membership a port supplies.
 *
 * The sweep deletes exactly the candidates it is given, one signed `delete` each with a
 * retention window of [retainDays] days, rather than calling `reject_sweep`, which trashes
 * every reject in the library: a reject synced in after the pass was read is not one the
 * user confirmed.
 *
 * The verbs are blocking, so they run on [io]. Any workspace failure, or a core that cannot
 * be reached (a [LinkageError]), is rethrown as [IllegalStateException].
 */
class FfiCullPort(
    private val workspace: FfiWorkspaceInterface,
    private val io: CoroutineDispatcher = Dispatchers.IO,
    private val retainDays: UInt = DEFAULT_RETAIN_DAYS,
) : CullPort {
    override suspend fun pass(): List<CullAsset> =
        call("read the pass") {
            CullFlag.entries
                .flatMap { flag -> workspace.assetsByCull(flag.toFfi()).map { CullAsset(AssetId(it), flag) } }
                .sortedBy { it.id.value }
        }.also { log.fine("culling: read ${it.size} assets") }

    override suspend fun setCull(
        asset: AssetId,
        flag: CullFlag,
    ) {
        call("flag an asset") { workspace.setCull(asset.value, flag.toFfi()) }
    }

    override suspend fun flagStack(
        stack: StackId,
        flag: CullFlag,
    ): List<AssetId> = call("flag a stack") { workspace.flagStack(stack.value, flag.toFfi()).map(::AssetId) }

    override suspend fun moveToTrash(assets: List<AssetId>): List<AssetId> =
        withContext(io) {
            val moved = mutableListOf<AssetId>()
            for (asset in assets) {
                try {
                    workspace.deleteAsset(asset.value, retainDays)
                } catch (error: FfiException) {
                    log.log(Level.WARNING, "culling: the sweep stopped after ${moved.size} assets", error)
                    throw SweepIncomplete(moved.toList(), error)
                } catch (error: LinkageError) {
                    log.log(Level.WARNING, "culling: the core's native library is unavailable", error)
                    throw SweepIncomplete(moved.toList(), error)
                }
                moved.add(asset)
            }
            log.info("culling: moved ${moved.size} rejects to the trash for $retainDays days")
            moved
        }

    private suspend fun <T> call(
        verb: String,
        body: () -> T,
    ): T =
        withContext(io) {
            try {
                body().also { log.fine("culling: $verb answered") }
            } catch (error: FfiException) {
                log.log(Level.WARNING, "culling: could not $verb", error)
                throw IllegalStateException("the workspace could not $verb", error)
            } catch (error: LinkageError) {
                log.log(Level.WARNING, "culling: the core's native library is unavailable", error)
                throw IllegalStateException("the workspace's native library is unavailable", error)
            }
        }

    companion object {
        /** The retention window a swept reject gets: the CLI's `cull sweep` default. */
        const val DEFAULT_RETAIN_DAYS: UInt = 30u

        private val log: Logger = Logger.getLogger("capsule.culling")

        fun CullFlag.toFfi(): FfiCullFlag =
            when (this) {
                CullFlag.PICK -> FfiCullFlag.PICK
                CullFlag.NEUTRAL -> FfiCullFlag.NEUTRAL
                CullFlag.REJECT -> FfiCullFlag.REJECT
            }
    }
}
