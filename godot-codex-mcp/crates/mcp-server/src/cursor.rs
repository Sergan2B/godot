use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const CURSOR_DOMAIN: &[u8] = b"godot-codex/resource-cursor/v1\0";
const CURSOR_TTL_SECONDS: u64 = 5 * 60;
const MAX_CURSOR_BYTES: usize = 4_096;
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceTool {
    Dependencies,
    Owners,
}

impl ResourceTool {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Dependencies => "godot_get_resource_dependencies",
            Self::Owners => "godot_find_resource_owners",
        }
    }
}

pub(crate) struct CursorBinding<'a> {
    pub project_id: &'a str,
    pub tool: ResourceTool,
    pub selector: &'a str,
    pub limit: usize,
    pub generation_id: &'a str,
    pub index_revision: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    version: u8,
    project_id: String,
    tool: String,
    selector: String,
    limit: usize,
    generation_id: String,
    index_revision: u64,
    offset: usize,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Clone)]
pub(crate) struct CursorCodec {
    secret: [u8; 32],
}

impl CursorCodec {
    pub(crate) fn new() -> Self {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).expect("OS randomness is required for MCP cursor integrity");
        Self { secret }
    }

    #[cfg(test)]
    fn from_secret(secret: [u8; 32]) -> Self {
        Self { secret }
    }

    pub(crate) fn issue(
        &self,
        binding: &CursorBinding<'_>,
        offset: usize,
        now: u64,
    ) -> Result<String, ()> {
        let payload = CursorPayload {
            version: 1,
            project_id: binding.project_id.to_owned(),
            tool: binding.tool.name().to_owned(),
            selector: binding.selector.to_owned(),
            limit: binding.limit,
            generation_id: binding.generation_id.to_owned(),
            index_revision: binding.index_revision,
            offset,
            issued_at: now,
            expires_at: now.checked_add(CURSOR_TTL_SECONDS).ok_or(())?,
        };
        let payload = serde_json::to_vec(&payload).map_err(|_| ())?;
        let payload_length = u32::try_from(payload.len()).map_err(|_| ())?;
        let mut mac = HmacSha256::new_from_slice(&self.secret).map_err(|_| ())?;
        mac.update(CURSOR_DOMAIN);
        mac.update(&payload_length.to_be_bytes());
        mac.update(&payload);
        let tag = mac.finalize().into_bytes();
        let mut token = Vec::with_capacity(4 + payload.len() + tag.len());
        token.extend_from_slice(&payload_length.to_be_bytes());
        token.extend_from_slice(&payload);
        token.extend_from_slice(&tag);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token))
    }

    pub(crate) fn validate(
        &self,
        token: &str,
        binding: &CursorBinding<'_>,
        now: u64,
    ) -> Result<usize, ()> {
        if token.is_empty() || token.len() > MAX_CURSOR_BYTES {
            return Err(());
        }
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| ())?;
        if decoded.len() < 4 + 32 || decoded.len() > MAX_CURSOR_BYTES {
            return Err(());
        }
        let payload_length = u32::from_be_bytes(decoded[..4].try_into().map_err(|_| ())?);
        let payload_length = usize::try_from(payload_length).map_err(|_| ())?;
        if decoded.len() != 4 + payload_length + 32 {
            return Err(());
        }
        let payload = &decoded[4..4 + payload_length];
        let tag = &decoded[4 + payload_length..];
        let mut mac = HmacSha256::new_from_slice(&self.secret).map_err(|_| ())?;
        mac.update(CURSOR_DOMAIN);
        mac.update(&decoded[..4]);
        mac.update(payload);
        mac.verify_slice(tag).map_err(|_| ())?;
        let payload: CursorPayload = serde_json::from_slice(payload).map_err(|_| ())?;
        if payload.version != 1
            || payload.project_id != binding.project_id
            || payload.tool != binding.tool.name()
            || payload.selector != binding.selector
            || payload.limit != binding.limit
            || payload.generation_id != binding.generation_id
            || payload.index_revision != binding.index_revision
            || payload.issued_at > now.saturating_add(5)
            || payload.expires_at < now
            || payload.expires_at.checked_sub(payload.issued_at) != Some(CURSOR_TTL_SECONDS)
        {
            return Err(());
        }
        Ok(payload.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(tool: ResourceTool) -> CursorBinding<'static> {
        CursorBinding {
            project_id: "project:test",
            tool,
            selector: "uid://a",
            limit: 50,
            generation_id: "generation:test",
            index_revision: 7,
        }
    }

    #[test]
    fn cursor_rejects_tampering_expiry_and_cross_tool_reuse() {
        let codec = CursorCodec::from_secret([7; 32]);
        let cursor = codec
            .issue(&binding(ResourceTool::Dependencies), 50, 1_000)
            .unwrap();
        assert_eq!(
            codec.validate(&cursor, &binding(ResourceTool::Dependencies), 1_001),
            Ok(50)
        );
        assert!(
            codec
                .validate(&cursor, &binding(ResourceTool::Owners), 1_001)
                .is_err()
        );
        assert!(
            codec
                .validate(&cursor, &binding(ResourceTool::Dependencies), 1_301)
                .is_err()
        );
        let mut tampered = cursor.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        assert!(
            codec
                .validate(
                    std::str::from_utf8(&tampered).unwrap(),
                    &binding(ResourceTool::Dependencies),
                    1_001
                )
                .is_err()
        );
    }
}
