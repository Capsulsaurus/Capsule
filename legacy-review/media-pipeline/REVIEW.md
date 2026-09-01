# Media Pipeline Review Notes

This is non-buildable reference material for the Rawshift integration. The implemented JPEG,
metadata, pixel-buffer, preset, and filesystem ideas may inform adapter tests, but Rawshift owns
all detection, decoding, encoding, metadata extraction, normalization, derivative generation,
preview extraction, resource limits, and video processing.

ThumbHash, empty per-format implementations, and placeholder video modules were deleted. Capsule
will import Chromahash directly after its v1 release; Chromahash is not a Rawshift responsibility.

No source here may be restored as a fallback codec. The Rawshift adapter must instead fail with a
structured error when a promised capability is unavailable.
