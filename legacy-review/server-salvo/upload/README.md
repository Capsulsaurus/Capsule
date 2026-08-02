# Capsule Upload API

The upload API asynchronously generates asset metadata and thumbnails, speeding up the upload-to-ready latency. Previously, assets would be passed to an asynchronous queue for eventual processing of all metadata and proxies (including non-essentials).

## Developer Notes

This package makes up a critical hot path of the Capsule API. As such all development on here are held at a higher standards of performance and correctness. Some general technical considerations:

- **Memory management:** Chunked uploads of large files could consume large amounts of hot memory. Offload compute and IO-heavy tasks with appropriate techniques for each.
- **File I/O:** Implementations should prefer optimized APIs. Since the target are modern Linux distributions, use of `io_uring` for file is expected. Improvements should be profiled and checked for regression.
- **Backpressure handling:** Since there are several task pools of varying characteristics and priorities executing (network I/O, file I/O, metadata/thumbnail processing), server should be tested under various load scenarios and emulated on reasonable hardware limits.
