//! Library-mode binding generator. Build the cdylib with `--features ffi`, then run this
//! (`--features ffi-bindgen`) against it to emit Kotlin/Swift; see the justfile `gen-bindings`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
