/*
 * CHECK-ONLY stand-in for Apple's <os/log.h>, which zig's bundled macOS libc headers omit.
 *
 * scripts/cross/zig-cc puts this directory on the include path for Apple targets so the `oslog`
 * crate's wrapper.c (pulled in by capsule-core-ffi) compiles under `mise run check-targets`.
 * The resulting object is never linked — `cargo check` only needs the build script to succeed —
 * so these declarations need to be shape-compatible, not behaviourally faithful. The real header
 * comes from the Xcode SDK in every `build-*` task.
 */
#ifndef CAPSULE_CHECK_STUB_OS_LOG_H
#define CAPSULE_CHECK_STUB_OS_LOG_H

#include <stdint.h>

typedef struct os_log_s *os_log_t;
typedef uint8_t os_log_type_t;

#define OS_LOG_TYPE_DEFAULT ((os_log_type_t)0x00)
#define OS_LOG_TYPE_INFO ((os_log_type_t)0x01)
#define OS_LOG_TYPE_DEBUG ((os_log_type_t)0x02)
#define OS_LOG_TYPE_ERROR ((os_log_type_t)0x10)
#define OS_LOG_TYPE_FAULT ((os_log_type_t)0x11)

#define OS_LOG_DEFAULT ((os_log_t)0)

os_log_t os_log_create(const char *subsystem, const char *category);

#define os_log_with_type(log, type, ...) ((void)(log), (void)(type))
#define os_log(log, ...) os_log_with_type(log, OS_LOG_TYPE_DEFAULT, __VA_ARGS__)
#define os_log_info(log, ...) os_log_with_type(log, OS_LOG_TYPE_INFO, __VA_ARGS__)
#define os_log_debug(log, ...) os_log_with_type(log, OS_LOG_TYPE_DEBUG, __VA_ARGS__)
#define os_log_error(log, ...) os_log_with_type(log, OS_LOG_TYPE_ERROR, __VA_ARGS__)
#define os_log_fault(log, ...) os_log_with_type(log, OS_LOG_TYPE_FAULT, __VA_ARGS__)

#endif
