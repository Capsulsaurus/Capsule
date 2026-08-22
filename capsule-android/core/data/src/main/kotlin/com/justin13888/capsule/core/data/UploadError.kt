package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures anywhere in the resumable upload protocol.
 *
 * The largest family by far, because upload is the one surface where a keyless server can check a
 * client's work in detail. Every mismatch it can detect gets its own code, so a client can tell a
 * retryable problem from a bug in its own bundle.
 */
sealed class UploadError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 403, invariant 6: album missing, no write capability, or album protocol version drift.
     */
    data object AlbumAccessDenied : UploadError(
        code = "error.upload.album_access_denied",
        detail = "You don't have permission to add photos to this album.",
    )

    /**
     * HTTP 400 on POST /albums/{album_id}/ops, invariant 18: the manifest's amk_version regresses
     * below the album's recorded epoch.
     */
    data object AmkRegressed : UploadError(
        code = "error.upload.amk_regressed",
        detail = "This change references an older album key than the server has recorded.",
    )

    /**
     * HTTP 400, invariant 12: SHA-256 of the received body doesn't match X-Capsule-Checksum.
     */
    data object ChecksumMismatch : UploadError(
        code = "error.upload.checksum_mismatch",
        detail = "Upload data was corrupted in transit. Retrying…",
    )

    /**
     * HTTP 409, invariant 12: a PATCH at an already-acknowledged offset with a different chunk hash
     * — the structural defense against a faulty client retrying with garbage.
     */
    data object ChunkConflict : UploadError(
        code = "error.upload.chunk_conflict",
        detail = "The upload sent conflicting data and was stopped.",
    )

    /**
     * HTTP 400, invariant 10: non-final chunk not a multiple of 4 KiB.
     */
    data object ChunkNotAligned : UploadError(
        code = "error.upload.chunk_not_aligned",
        detail = "An upload chunk was incorrectly sized.",
    )

    /**
     * HTTP 413, invariant 10: chunk body larger than the 16 MiB protocol maximum.
     */
    data object ChunkTooLarge : UploadError(
        code = "error.upload.chunk_too_large",
        detail = "An upload chunk exceeded the maximum chunk size.",
    )

    /**
     * HTTP 400 at finalization, invariant 14: recomputed ciphertext hash differs from the declared
     * hash.
     */
    data object ContentHashMismatch : UploadError(
        code = "error.upload.content_hash_mismatch",
        detail = "The upload failed integrity verification and was discarded.",
    )

    /**
     * HTTP 403, invariant 7: manifest envelope's created_by_device is not in the uploader's device
     * directory.
     */
    data object DeviceNotAuthorized : UploadError(
        code = "error.upload.device_not_authorized",
        detail = "This device isn't authorized to upload. Re-enroll it and try again.",
    )

    /**
     * HTTP 409 on POST /upload when the content hash is already finalized for this owner+album.
     */
    data object DuplicateBlob : UploadError(
        code = "error.upload.duplicate_blob",
        detail = "This file is already stored.",
    )

    /**
     * HTTP 400, invariant 10: zero-length PATCH body. Always a client bug; never accepted as a no-
     * op.
     */
    data object EmptyChunk : UploadError(
        code = "error.upload.empty_chunk",
        detail = "An upload chunk cannot be empty.",
    )

    /**
     * HTTP 400: top-level request fields (crypto_suite_id, protocol_version, album_id) disagree with
     * the manifest_envelope.
     */
    data object EnvelopeMismatch : UploadError(
        code = "error.upload.envelope_mismatch",
        detail = "The upload request contradicts its signed manifest.",
    )

    /**
     * HTTP 400 at finalization, invariant 15: envelope re-validation (rerun of invariants 1–8)
     * failed inside the finalization transaction.
     */
    data object EnvelopeRejected : UploadError(
        code = "error.upload.envelope_rejected",
        detail = "The upload's signed manifest failed validation.",
    )

    /**
     * HTTP 413, invariant 4: declared size (or cumulative bytes) exceeds max_file_size.
     */
    data object FileTooLarge : UploadError(
        code = "error.upload.file_too_large",
        detail = "This file exceeds the server's size limit.",
    )

    /**
     * HTTP 409: a concurrent finalization attempt lost the atomic status compare-and-set.
     */
    data object FinalizeInProgress : UploadError(
        code = "error.upload.finalize_in_progress",
        detail = "This upload is already being finalized.",
    )

    /**
     * HTTP 403: caller is neither the session's uploader nor the asset owner.
     */
    data object Forbidden : UploadError(
        code = "error.upload.forbidden",
        detail = "You don't have access to this upload.",
    )

    /**
     * HTTP 400 on POST /albums/{album_id}/ops, invariant 16: the manifest's action is outside the
     * closed lifecycle-action set, or is create/replace (which ride the upload protocol, not this
     * surface).
     */
    data object InvalidAction : UploadError(
        code = "error.upload.invalid_action",
        detail = "This change uses an action the album doesn't allow.",
    )

    /**
     * HTTP 400, invariant 3: hash is not lowercase hex or its length doesn't match the suite's
     * digest length.
     */
    data object InvalidHash : UploadError(
        code = "error.upload.invalid_hash",
        detail = "The upload's declared content hash is invalid.",
    )

    /**
     * HTTP 400, invariant 4: declared size is zero or negative.
     */
    data object InvalidSize : UploadError(
        code = "error.upload.invalid_size",
        detail = "The upload's declared size is invalid.",
    )

    /**
     * HTTP 400 on POST /upload: unknown or missing JSON fields. The transport JSON is strict (deny
     * unknown fields).
     */
    data object MalformedRequest : UploadError(
        code = "error.upload.malformed_request",
        detail = "The upload request was malformed.",
    )

    /**
     * HTTP 400, invariant 12: X-Capsule-Checksum header missing or not valid lowercase-hex SHA-256.
     */
    data object MissingChecksum : UploadError(
        code = "error.upload.missing_checksum",
        detail = "The upload request was missing its integrity checksum.",
    )

    /**
     * HTTP 400, invariant 9 family: X-Capsule-Offset header missing or unparseable on PATCH.
     */
    data object MissingOffset : UploadError(
        code = "error.upload.missing_offset",
        detail = "The upload request was missing its position marker.",
    )

    /**
     * HTTP 409, invariant 9: PATCH offset differs from the current received-byte count.
     */
    data object OffsetMismatch : UploadError(
        code = "error.upload.offset_mismatch",
        detail = "The upload got out of sync and is realigning.",
    )

    /**
     * HTTP 403: owner_id differs from the uploader without a verified relationship.
     */
    data object OwnerNotPermitted : UploadError(
        code = "error.upload.owner_not_permitted",
        detail = "You can't upload on behalf of this user.",
    )

    /**
     * HTTP 409, invariant 33: GET /upload/{id}/receipt before the session reached Completed.
     */
    data object ReceiptNotAvailable : UploadError(
        code = "error.upload.receipt_not_available",
        detail = "The upload hasn't finished, so its storage receipt isn't available yet.",
    )

    /**
     * HTTP 409: PATCH/DELETE against a session that is finalizing or terminal. The outcome is read
     * via HEAD.
     */
    data object SessionNotActive : UploadError(
        code = "error.upload.session_not_active",
        detail = "This upload has already finished.",
    )

    /**
     * HTTP 404: unknown, expired, or pressure-discarded session — deliberately indistinguishable.
     */
    data object SessionNotFound : UploadError(
        code = "error.upload.session_not_found",
        detail = "The upload session expired. Starting over…",
    )

    /**
     * HTTP 400, invariant 11: cumulative received bytes would exceed the declared size.
     */
    data object SizeExceeded : UploadError(
        code = "error.upload.size_exceeded",
        detail = "The upload sent more data than it declared.",
    )

    /**
     * HTTP 409 on POST /albums/{album_id}/ops, invariant 17: the manifest's prior_provenance_hash
     * does not match the asset's current provenance-chain head (a stale or forked position, e.g.
     */
    data object StaleRevival : UploadError(
        code = "error.upload.stale_revival",
        detail = "This change is based on an out-of-date version of the item.",
    )

    /**
     * HTTP 500: the upload file's on-disk length diverged from the session's expected offset.
     */
    data object StorageInconsistent : UploadError(
        code = "error.upload.storage_inconsistent",
        detail = "The server hit a storage problem. Please try again.",
    )

    /**
     * HTTP 400, invariant 8: envelope timestamp outside the gross-drift sanity window (default ±30
     * days).
     */
    data object TimestampOutOfRange : UploadError(
        code = "error.upload.timestamp_out_of_range",
        detail = "Your device's clock appears to be wrong. Check its date and time.",
    )

    /**
     * HTTP 400, invariant 2: crypto_suite_id not in the server's inventory.
     */
    data object UnknownCryptoSuite : UploadError(
        code = "error.upload.unknown_crypto_suite",
        detail = "This app version uses an encryption suite the server doesn't recognize.",
    )

    /**
     * HTTP 400, invariant 5: content_type outside the closed enum for the pinned protocol version.
     */
    data object UnsupportedContentType : UploadError(
        code = "error.upload.unsupported_content_type",
        detail = "This file type is not supported.",
    )

    /**
     * HTTP 415, invariant 10: PATCH body Content-Type is not application/octet-stream.
     */
    data object UnsupportedMediaType : UploadError(
        code = "error.upload.unsupported_media_type",
        detail = "Upload data was sent in an unsupported format.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<UploadError> =
            listOf(
                AlbumAccessDenied,
                AmkRegressed,
                ChecksumMismatch,
                ChunkConflict,
                ChunkNotAligned,
                ChunkTooLarge,
                ContentHashMismatch,
                DeviceNotAuthorized,
                DuplicateBlob,
                EmptyChunk,
                EnvelopeMismatch,
                EnvelopeRejected,
                FileTooLarge,
                FinalizeInProgress,
                Forbidden,
                InvalidAction,
                InvalidHash,
                InvalidSize,
                MalformedRequest,
                MissingChecksum,
                MissingOffset,
                OffsetMismatch,
                OwnerNotPermitted,
                ReceiptNotAvailable,
                SessionNotActive,
                SessionNotFound,
                SizeExceeded,
                StaleRevival,
                StorageInconsistent,
                TimestampOutOfRange,
                UnknownCryptoSuite,
                UnsupportedContentType,
                UnsupportedMediaType,
            )
    }
}
