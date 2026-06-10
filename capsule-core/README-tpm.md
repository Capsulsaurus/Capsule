# TPM device signer — building & testing

`capsule_core::crypto::keys::tpm::TpmSigner` is the desktop (Linux / Windows) TPM 2.0 reference
`HardwareSigner`. It is behind the off-by-default `tpm` feature and **not** built in CI, because it
links the system `libtss2` and needs a real TPM (or a software TPM) to run.

## Build (a TPM-capable host: Linux or Windows)

```sh
# Debian/Ubuntu: the tss-esapi build needs the TSS2 headers + bindgen's clang.
sudo apt-get install -y libtss2-dev pkg-config clang
cargo build -p capsule-core --features tpm
```

On macOS there is no TPM; the reference is verified to compile + link on Linux. (See the algorithm
caveat in `src/crypto/keys/tpm.rs`: shipping TPMs do ECDSA-P256, not Ed25519, so this backend
awaits the P-256 hybrid-DSK variant before it composes into the device key — see `DEFERRED.md`.)

## Smoke test with a software TPM (`swtpm`)

```sh
sudo apt-get install -y swtpm swtpm-tools tpm2-tools

# Start an emulated TPM listening on a socket.
mkdir -p /tmp/capsule-swtpm
swtpm socket --tpm2 --tpmstate dir=/tmp/capsule-swtpm \
  --ctrl type=tcp,port=2322 --server type=tcp,port=2321 --flags not-need-init &

# Point tss-esapi at it, then run an enroll → sign → verify round trip.
export TPM2TOOLS_TCTI="swtpm:host=127.0.0.1,port=2321"
# In Rust: TpmSigner::from_environment()? -> enroll("device-dsk") -> sign_classical(..) ->
#          assert_non_exportable("device-dsk") (Ok: fixedTPM|fixedParent are set).
```

A `#[cfg(feature = "tpm")]` integration test driving exactly that round trip against `swtpm` is the
remaining follow-up; today the reference is gated at compile+link on Linux.
