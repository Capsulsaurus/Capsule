//! Library-mode binding generator for the SDK's uniffi surface.
//!
//! Build the cdylib with `--features ffi`, then run this (`--features ffi-bindgen`)
//! against it to emit the Swift/Kotlin sources; see the `gen-bindings` mise task.
//! Mirrors `capsule-core`'s generator so both surfaces share one bindings strategy.

fn main() {
    uniffi::uniffi_bindgen_main()
}
