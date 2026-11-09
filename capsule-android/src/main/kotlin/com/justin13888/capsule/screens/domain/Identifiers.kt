package com.justin13888.capsule.screens.domain

/** An asset of the library. */
@JvmInline
value class AssetId(
    val value: String,
)

/** A stack: assets grouped as one (a burst, a RAW+JPEG pair). */
@JvmInline
value class StackId(
    val value: String,
)

/** A container album. */
@JvmInline
value class AlbumId(
    val value: String,
)

/**
 * A labelled region of an image (*Asset Organization - Labels*). UUIDv7, minted fresh
 * per create, so a deleted region's id is never reused.
 */
@JvmInline
value class RegionId(
    val value: String,
)

/** A blob's content address, lowercase hex. */
@JvmInline
value class BlobHash(
    val value: String,
)

/** An upload session the server opened. */
@JvmInline
value class UploadId(
    val value: String,
)
