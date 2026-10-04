use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};

type HmacSha256 = Hmac<Sha256>;

pub(super) fn encode_token(key: &[u8; 32], version: &FileVersion) -> RuntimeResult<String> {
    let payload = serde_json::to_vec(version)
        .map_err(|error| RuntimeError::new("versionUnsupported", error.to_string()))?;
    let encoded = URL_SAFE_NO_PAD.encode(payload);
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| RuntimeError::new("versionUnsupported", "invalid version signing key"))?;
    mac.update(encoded.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("v{TOKEN_VERSION}:{encoded}:{signature}"))
}

pub(super) fn decode_token(key: &[u8; 32], token: &str) -> RuntimeResult<FileVersion> {
    let mut parts = token.split(':');
    let version = parts.next();
    let payload = parts.next();
    let signature = parts.next();
    if version != Some("v1") || parts.next().is_some() {
        return Err(RuntimeError::new(
            "versionUnsupported",
            "unsupported version token",
        ));
    }
    let (Some(payload), Some(signature)) = (payload, signature) else {
        return Err(RuntimeError::new(
            "versionUnsupported",
            "malformed version token",
        ));
    };
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| RuntimeError::new("versionUnsupported", "malformed version signature"))?;
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| RuntimeError::new("versionUnsupported", "invalid version signing key"))?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).map_err(|_| {
        RuntimeError::new("versionUnsupported", "version token authentication failed")
    })?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| RuntimeError::new("versionUnsupported", "malformed version payload"))?;
    let decoded: FileVersion = serde_json::from_slice(&bytes)
        .map_err(|_| RuntimeError::new("versionUnsupported", "invalid version payload"))?;
    if decoded.schema != TOKEN_VERSION {
        return Err(RuntimeError::new(
            "versionUnsupported",
            "unsupported version schema",
        ));
    }
    Ok(decoded)
}
