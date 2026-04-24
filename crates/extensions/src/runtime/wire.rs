//! JSON-RPC 2.0 framing over line-delimited stdio.

use super::RpcError;

#[derive(Debug, serde::Serialize)]
pub struct Request<'a, P: serde::Serialize> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    pub params: P,
    pub id: u64,
}

#[derive(Debug, serde::Deserialize)]
pub struct Response {
    #[serde(default)]
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

pub fn encode<P: serde::Serialize>(
    method: &str,
    params: P,
    id: u64,
) -> Result<String, serde_json::Error> {
    let req = Request { jsonrpc: "2.0", method, params, id };
    let mut s = serde_json::to_string(&req)?;
    s.push('\n');
    Ok(s)
}

pub fn decode_response(line: &str) -> Result<Response, serde_json::Error> {
    let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
    serde_json::from_str(trimmed)
}

pub fn extract_u64_id(id: &serde_json::Value) -> Option<u64> {
    id.as_u64()
        .or_else(|| id.as_i64().and_then(|n| u64::try_from(n).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_adds_newline_and_wraps_jsonrpc() {
        let out = encode("initialize", serde_json::json!({"agent_version":"0.1.0"}), 1).unwrap();
        assert!(out.ends_with('\n'));
        assert!(out.contains("\"jsonrpc\":\"2.0\""));
        assert!(out.contains("\"method\":\"initialize\""));
        assert!(out.contains("\"id\":1"));
    }

    #[test]
    fn decode_response_strips_crlf() {
        let line = "{\"jsonrpc\":\"2.0\",\"result\":{\"ok\":true},\"id\":7}\r\n";
        let r = decode_response(line).unwrap();
        assert_eq!(r.id.as_ref().and_then(extract_u64_id), Some(7));
        assert!(r.result.is_some());
        assert!(r.error.is_none());
    }

    #[test]
    fn decode_response_rejects_garbage() {
        assert!(decode_response("not json at all").is_err());
    }

    #[test]
    fn extract_u64_id_from_numbers() {
        assert_eq!(extract_u64_id(&serde_json::json!(42)), Some(42));
        assert_eq!(extract_u64_id(&serde_json::json!(-1)), None);
        assert_eq!(extract_u64_id(&serde_json::json!("42")), None);
    }
}
