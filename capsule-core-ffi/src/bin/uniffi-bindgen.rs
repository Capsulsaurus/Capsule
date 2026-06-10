//! Standalone `uniffi-bindgen` entry point.
//!
//! Invoked by `Scripts/build-rust-ffi.sh` to generate the Swift bindings from
//! the compiled `capsule-core-ffi` library:
//!
//! ```sh
//! cargo run --bin uniffi-bindgen -- generate --library <path> --language swift ...
//! ```
fn main() {
    uniffi::uniffi_bindgen_main();
}
