# Local rmcp patch

This directory vendors `rmcp` 2.2.0 from crates.io
([upstream repository](https://github.com/modelcontextprotocol/rust-sdk)),
originally published under Apache-2.0 with checksum
`14db48ee17a9ba61810ab1a9c1beb7d06d8136ae39ac25a1137f10d357af01af`.
The complete license text is retained in `LICENSE-APACHE`.

The single behavioral patch is in `src/model.rs`: deserialization of a present
`ElicitResult.content` preserves JSON `null` as `Some(Value::Null)`. Upstream's
derived `Option<Value>` deserializer otherwise maps both an omitted field and an
explicit `null` to `None`. That ambiguity is unsafe for the strict action-only
approval boundary, which accepts omission but rejects `null`.

The pinned SDK exposes typed transport messages but no raw-response validation
hook before `ElicitResult` deserialization, so this is the narrowest place to
retain the wire distinction. No dependency versions or SDK capabilities were
changed.

The public field type and serialized wire format are unchanged. A regression
test in `tests/test_elicitation.rs` covers omitted, explicit-null, and empty
object content. The only ancillary source change feature-gates two prompt imports
to keep the workspace's `default-features = false` build warning-free.
