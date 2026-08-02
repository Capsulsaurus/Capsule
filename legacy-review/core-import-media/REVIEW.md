# Client Import Media Review Notes

The archived executor mixed Capsule's import transaction with direct EXIF parsing and a hard-coded
Rawshift version. It is not an active API.

The replacement executor must consume normalized Rawshift results, call Chromahash directly,
apply Capsule privacy and sidecar policy, and only then encrypt, sign, and commit the asset. The
active scanner, grouping, planner, cryptography, sidecars, lifecycle, and local catalog remain the
contractual building blocks.
