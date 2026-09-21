# Focused Blame product-health fixtures

`minimal-current.valid.json` is a hand-authored exact fixture for the frozen
focused V1 contract. It is not release evidence. The byte-for-byte legacy Rust
proof vector is retained here as `legacy-rust-proof.json`; the Worker parity
test consumes its exact `body_utf8` and proof fields without rewriting them.
The fetch parity case runs with
one injected clock matching the fixture's proof timestamp and event minute;
temporal incoherence fails the qualification instead of being skipped. Current
semantics require `blame_target_kind` while forbidding all target values,
paths, refs, hashes, and selectors.
