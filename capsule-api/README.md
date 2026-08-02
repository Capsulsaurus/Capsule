# Capsule API

The previous Salvo server is quarantined under `legacy-review/server-salvo/` and is not part of
the Cargo workspace. It exposed contracts that conflict with Capsule's end-to-end encryption
model and must not be deployed.

The replacement server will use Kynos and expose REST/OpenAPI only. Its contracts must be defined
and tested before implementation is restored here.
