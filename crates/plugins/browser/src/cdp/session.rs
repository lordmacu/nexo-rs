use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Value, json};

use super::client::CdpClient;

pub struct CdpSession {
    client: Arc<CdpClient>,
    pub target_id: String,
    session_id: String,
    command_timeout: Duration,
}

impl CdpSession {
    pub(crate) fn client(&self) -> Arc<CdpClient> {
        Arc::clone(&self.client)
    }

    pub async fn new(
        client: Arc<CdpClient>,
        target_id: &str,
        command_timeout_ms: u64,
    ) -> anyhow::Result<Self> {
        let result = client.send(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        ).await?;

        let session_id = result
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no sessionId in Target.attachToTarget response"))?
            .to_string();

        Ok(Self {
            client,
            target_id: target_id.to_string(),
            session_id,
            command_timeout: Duration::from_millis(command_timeout_ms),
        })
    }

    async fn cdp(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        tokio::time::timeout(
            self.command_timeout,
            self.client.send_with_session(method, params, Some(self.session_id.clone())),
        )
        .await
        .map_err(|_| anyhow!("CDP command '{method}' timed out"))?
    }

    pub async fn navigate(&mut self, url: &str) -> anyhow::Result<()> {
        self.cdp("Page.enable", json!({})).await?;
        // Subscribe BEFORE sending Page.navigate so we don't race the event.
        let mut events = self.client.subscribe_session_events(&self.session_id);
        self.cdp("Page.navigate", json!({ "url": url })).await?;

        // Wait for the real load event. Cap with command_timeout to avoid
        // hanging on pages that never fire loadEventFired (e.g. infinite SPAs).
        let wait = async {
            loop {
                match events.recv().await {
                    Ok(ev) if ev.method == "Page.loadEventFired" => return Ok(()),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("CDP event stream closed before Page.loadEventFired"));
                    }
                }
            }
        };
        tokio::time::timeout(self.command_timeout, wait)
            .await
            .map_err(|_| anyhow!("navigate timed out waiting for Page.loadEventFired"))?
    }

    pub async fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        let result = self.cdp("Page.captureScreenshot", json!({ "format": "png" })).await?;
        let b64 = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no data in screenshot response"))?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow!("base64 decode failed: {e}"))
    }

    pub async fn evaluate(&self, script: &str) -> anyhow::Result<Value> {
        let result = self.cdp("Runtime.evaluate", json!({
            "expression": script,
            "returnByValue": true,
            "awaitPromise": true,
        })).await?;
        if let Some(exc) = result.get("exceptionDetails") {
            return Err(anyhow!("JS exception: {exc}"));
        }
        Ok(result.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
    }

    pub async fn snapshot(&mut self) -> anyhow::Result<String> {
        let result = self.cdp("Runtime.evaluate", json!({
            "expression": r#"
                (function() {
                    const tags = ['a','button','input','select','textarea','[role="button"]','[role="link"]'];
                    const elements = document.querySelectorAll(tags.join(','));
                    const refs = [];
                    elements.forEach((el, i) => {
                        const ref_id = '@e' + (i + 1);
                        el.setAttribute('data-agent-ref', ref_id);
                        refs.push({
                            ref_id,
                            tag: el.tagName.toLowerCase(),
                            text: (el.innerText || el.value || el.getAttribute('placeholder') || '').trim().slice(0, 80),
                            type: el.getAttribute('type') || '',
                        });
                    });
                    return JSON.stringify(refs);
                })()
            "#,
            "returnByValue": true,
        })).await?;

        let refs_json = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("[]");

        let refs: Vec<Value> = serde_json::from_str(refs_json)?;
        let mut lines = vec!["# Page snapshot".to_string()];

        for r in &refs {
            let ref_id = r["ref_id"].as_str().unwrap_or("");
            let tag = r["tag"].as_str().unwrap_or("");
            let text = r["text"].as_str().unwrap_or("");
            let typ = r["type"].as_str().unwrap_or("");

            let label = if typ.is_empty() {
                format!("[{tag}] {text} — {ref_id}")
            } else {
                format!("[{tag}:{typ}] {text} — {ref_id}")
            };
            lines.push(label);
        }

        Ok(lines.join("\n"))
    }

    pub async fn click(&self, target: &str) -> anyhow::Result<()> {
        let script = if target.starts_with('@') {
            format!(
                r#"document.querySelector('[data-agent-ref="{target}"]')?.click()"#
            )
        } else {
            format!(r#"document.querySelector({:?})?.click()"#, target)
        };
        self.evaluate(&script).await?;
        Ok(())
    }

    pub async fn fill(&self, target: &str, value: &str) -> anyhow::Result<()> {
        let escaped_value = value.replace('\\', "\\\\").replace('"', "\\\"");
        let script = if target.starts_with('@') {
            format!(
                r#"
                var el = document.querySelector('[data-agent-ref="{target}"]');
                if (el) {{ el.focus(); el.value = "{escaped_value}"; el.dispatchEvent(new Event('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); }}
                "#
            )
        } else {
            format!(
                r#"
                var el = document.querySelector({:?});
                if (el) {{ el.focus(); el.value = "{escaped_value}"; el.dispatchEvent(new Event('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); }}
                "#,
                target
            )
        };
        self.evaluate(&script).await?;
        Ok(())
    }

    pub async fn scroll_to(&self, target: &str) -> anyhow::Result<()> {
        let script = if target.starts_with('@') {
            format!(
                r#"document.querySelector('[data-agent-ref="{target}"]')?.scrollIntoView({{behavior:'smooth',block:'center'}})"#
            )
        } else {
            format!(
                r#"document.querySelector({:?})?.scrollIntoView({{behavior:'smooth',block:'center'}})"#,
                target
            )
        };
        self.evaluate(&script).await?;
        Ok(())
    }
}
