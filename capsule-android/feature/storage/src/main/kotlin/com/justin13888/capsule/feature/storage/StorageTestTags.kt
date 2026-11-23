package com.justin13888.capsule.feature.storage

/** Test handle for the quota state card, whichever of the five states it is showing. */
const val QUOTA_STATE_TAG: String = "storage.quota.state"

/** Test handle for the quota usage bar. */
const val QUOTA_BAR_TAG: String = "storage.quota.bar"

/** Test handle for the standing reassurance that deletes are never blocked. */
const val QUOTA_DELETES_TAG: String = "storage.quota.deletes"

/** Test handle for the local-storage screen as a whole. */
const val STORAGE_SCREEN_TAG: String = "storage.local"

/** Test handle for the cache budget section. */
const val STORAGE_BUDGET_TAG: String = "storage.budget"

/** Test handle for the sync scope section. */
const val STORAGE_SCOPE_TAG: String = "storage.scope"

/** Test handle for the biggest-consumers list. */
const val STORAGE_CONSUMERS_TAG: String = "storage.consumers"

/** Test handle for the empty state of the consumers list. */
const val STORAGE_EMPTY_TAG: String = "storage.empty"

/** Test handle for the loading state. */
const val STORAGE_LOADING_TAG: String = "storage.loading"

/** Test handle for the error state. */
const val STORAGE_ERROR_TAG: String = "storage.error"

/** Test handle for one consumer row: `storage.consumer:<asset id>`. */
fun storageConsumerTag(assetId: String): String = "storage.consumer:$assetId"

/** Test handle for the unconfirmed-release notice on one row. */
fun storageUnconfirmedTag(assetId: String): String = "storage.unconfirmed:$assetId"
