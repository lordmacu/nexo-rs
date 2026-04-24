//! Parse `sampling/createMessage` params from JSON-RPC and encode
//! [`SamplingResponse`] into the `result` object the server expects.

use serde_json::{json, Value};

use super::types::{
    IncludeContext, ModelPreferences, SamplingMessage, SamplingRequest, SamplingResponse,
    SamplingRole,
};
use super::SamplingError;

const DEFAULT_MAX_TOKENS: u32 = 1024;

pub fn parse_create_message_params(
    server_id: &str,
    params: &Value,
) -> Result<SamplingRequest, SamplingError> {
    let messages_json = params
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| SamplingError::InvalidParams("missing `messages` array".into()))?;
    if messages_json.is_empty() {
        return Err(SamplingError::InvalidParams("`messages` is empty".into()));
    }
    let mut messages = Vec::with_capacity(messages_json.len());
    for m in messages_json {
        let role_s = m
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| SamplingError::InvalidParams("message missing `role`".into()))?;
        let role = match role_s {
            "user" => SamplingRole::User,
            "assistant" => SamplingRole::Assistant,
            other => {
                return Err(SamplingError::InvalidParams(format!(
                    "unsupported role `{other}`"
                )))
            }
        };
        let content = m
            .get("content")
            .ok_or_else(|| SamplingError::InvalidParams("message missing `content`".into()))?;
        let ctype = content.get("type").and_then(Value::as_str).unwrap_or("");
        if ctype != "text" {
            return Err(SamplingError::InvalidParams(format!(
                "content type `{ctype}` not supported (text only in MVP)"
            )));
        }
        let text = content
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| SamplingError::InvalidParams("content missing `text`".into()))?
            .to_string();
        messages.push(SamplingMessage { role, text });
    }

    let model_preferences = params.get("modelPreferences").map(|mp| {
        let hints = mp
            .get("hints")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ModelPreferences {
            hints,
            cost_priority: mp
                .get("costPriority")
                .and_then(Value::as_f64)
                .map(|v| v as f32),
            speed_priority: mp
                .get("speedPriority")
                .and_then(Value::as_f64)
                .map(|v| v as f32),
            intelligence_priority: mp
                .get("intelligencePriority")
                .and_then(Value::as_f64)
                .map(|v| v as f32),
        }
    });

    let system_prompt = params
        .get("systemPrompt")
        .and_then(Value::as_str)
        .map(str::to_string);
    let include_context = params
        .get("includeContext")
        .and_then(Value::as_str)
        .map(IncludeContext::parse)
        .unwrap_or(IncludeContext::None);
    let temperature = params
        .get("temperature")
        .and_then(Value::as_f64)
        .map(|v| v as f32);
    let max_tokens = params
        .get("maxTokens")
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let stop_sequences = params
        .get("stopSequences")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let metadata = params.get("metadata").cloned().unwrap_or(Value::Null);

    Ok(SamplingRequest {
        server_id: server_id.to_string(),
        messages,
        model_preferences,
        system_prompt,
        include_context,
        temperature,
        max_tokens,
        stop_sequences,
        metadata,
    })
}

pub fn encode_response(resp: &SamplingResponse) -> Value {
    json!({
        "role": resp.role.as_str(),
        "content": {
            "type": "text",
            "text": resp.text,
        },
        "model": resp.model,
        "stopReason": resp.stop_reason.as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::types::StopReason;
    use super::*;

    #[test]
    fn parses_text_messages() {
        let p = json!({
            "messages": [{
                "role": "user",
                "content": {"type":"text","text":"hola"}
            }],
            "maxTokens": 256,
            "temperature": 0.3,
        });
        let r = parse_create_message_params("srv", &p).unwrap();
        assert_eq!(r.server_id, "srv");
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.messages[0].text, "hola");
        assert_eq!(r.max_tokens, 256);
        assert_eq!(r.temperature, Some(0.3));
    }

    #[test]
    fn rejects_image_content() {
        let p = json!({
            "messages": [{"role":"user","content":{"type":"image","data":"..."}}],
        });
        let err = parse_create_message_params("srv", &p).unwrap_err();
        assert!(matches!(err, SamplingError::InvalidParams(_)));
    }

    #[test]
    fn rejects_missing_messages() {
        let err = parse_create_message_params("srv", &json!({})).unwrap_err();
        assert!(matches!(err, SamplingError::InvalidParams(_)));
    }

    #[test]
    fn rejects_empty_messages() {
        let p = json!({"messages":[]});
        let err = parse_create_message_params("srv", &p).unwrap_err();
        assert!(matches!(err, SamplingError::InvalidParams(_)));
    }

    #[test]
    fn parses_model_preferences() {
        let p = json!({
            "messages":[{"role":"user","content":{"type":"text","text":"x"}}],
            "modelPreferences": {
                "hints": [{"name":"gpt-5"}, {"name":"minimax-m2"}],
                "costPriority": 0.2,
                "speedPriority": 0.5,
                "intelligencePriority": 0.9,
            }
        });
        let r = parse_create_message_params("srv", &p).unwrap();
        let mp = r.model_preferences.unwrap();
        assert_eq!(mp.hints, vec!["gpt-5".to_string(), "minimax-m2".into()]);
        assert_eq!(mp.cost_priority, Some(0.2));
        assert_eq!(mp.speed_priority, Some(0.5));
        assert_eq!(mp.intelligence_priority, Some(0.9));
    }

    #[test]
    fn encodes_response_shape() {
        let r = SamplingResponse {
            role: SamplingRole::Assistant,
            text: "ok".into(),
            model: "minimax".into(),
            stop_reason: StopReason::EndTurn,
        };
        let v = encode_response(&r);
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"]["type"], "text");
        assert_eq!(v["content"]["text"], "ok");
        assert_eq!(v["model"], "minimax");
        assert_eq!(v["stopReason"], "endTurn");
    }

    #[test]
    fn parses_stop_sequences_and_include_context() {
        let p = json!({
            "messages":[{"role":"user","content":{"type":"text","text":"x"}}],
            "stopSequences": ["\n\n", "END"],
            "includeContext": "thisServer",
        });
        let r = parse_create_message_params("srv", &p).unwrap();
        assert_eq!(r.stop_sequences.len(), 2);
        assert_eq!(r.include_context, IncludeContext::ThisServer);
    }
}
