package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.domain.TransferEventPort

/**
 * The mock lane's world for the transfer view and the label editor: one sample asset,
 * stack and album, a label store that knows exactly those, and the recorded transfer
 * feed. Every id is a function of [seed], so two environments built alike are the same
 * world. The app runs on this until the Rust-backed adapters over `capsule_sdk` land.
 */
class MockEnvironment(
    val seed: Long = DEFAULT_SEED,
) {
    /** The asset the sample viewer opens. */
    val sampleAsset: AssetId = MockIdentifiers.assetId(seed, 0)

    /** The stack the stack label editor opens. */
    val sampleStack: StackId = MockIdentifiers.stackId(seed, 0)

    /** The album the album label editor opens. */
    val sampleAlbum: AlbumId = MockIdentifiers.albumId(seed, 0)

    /** Labels and regions, held for the life of the environment. */
    val labels: LabelPort =
        MockLabelStore(
            MockLabelStore.Existence(
                asset = { it == sampleAsset },
                stack = { it == sampleStack },
                album = { it == sampleAlbum },
            ),
            seed,
        )

    /** The recorded transfer stream, replayed to each subscriber. */
    val transferEvents: TransferEventPort = MockTransferEventFeed.demo(seed)

    companion object {
        const val DEFAULT_SEED = 1L
    }
}
