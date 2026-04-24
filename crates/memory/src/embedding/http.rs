//! OpenAI-compatible HTTP embedding provider.
//!
//! Works with any server that serves `/embeddings` following the OpenAI
//! spec: Ollama, LocalAI, llama-server, vLLM, the real OpenAI API, etc.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use lru::LruCache;
use serde::Deserialize;

use super::EmbeddingProvider;

pub struct HttpEmbeddingProvider {
    client: reqwest::Client,
    base_url: url::Url,
    model: String,
    api_key: Option<String>,
    dimension: usize,
    cache: Mutex<LruCache<String, Vec<f32>>>,
}

impl HttpEmbeddingProvider {
    pub fn new(
        base_url: url::Url,
        model: String,
        api_key: Option<String>,
        dimension: usize,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        if model.trim().is_empty() {
            anyhow::bail!("embedding model must not be empty");
        }
        if dimension == 0 {
            anyhow::bail!("embedding dimension must be > 0");
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            base_url,
            model,
            api_key,
            dimension,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(64).unwrap())),
        })
    }

    fn endpoint(&self) -> url::Url {
        // Respect an existing path on base_url ("http://host/v1") and
        // append "embeddings". `join` strips the final segment unless it
        // ends in "/", so normalize first.
        let base = if self.base_url.path().ends_with('/') {
            self.base_url.clone()
        } else {
            let mut b = self.base_url.clone();
            let p = format!("{}/", b.path());
            b.set_path(&p);
            b
        };
        base.join("embeddings").unwrap_or(base)
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

#[async_trait]
impl EmbeddingProvider for HttpEmbeddingProvider {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Single-query fast path: consult cache.
        if texts.len() == 1 {
            if let Ok(mut cache) = self.cache.lock() {
                if let Some(v) = cache.get(texts[0]) {
                    return Ok(vec![v.clone()]);
                }
            }
        }

        let endpoint = self.endpoint();
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let mut req = self.client.post(endpoint.clone()).json(&body);
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let response = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("embedding request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("embedding server responded {status}: {text}");
        }

        let parsed: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("invalid embedding response: {e}"))?;
        if parsed.data.len() != texts.len() {
            anyhow::bail!(
                "embedding response size mismatch: got {}, expected {}",
                parsed.data.len(),
                texts.len()
            );
        }
        // Sort by `index` so the response order matches the request order
        // regardless of server implementation.
        let mut items = parsed.data;
        items.sort_by_key(|i| i.index);
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(items.len());
        for item in items {
            if item.embedding.len() != self.dimension {
                anyhow::bail!(
                    "embedding dimension mismatch: got {}, expected {}",
                    item.embedding.len(),
                    self.dimension
                );
            }
            out.push(item.embedding);
        }

        if texts.len() == 1 {
            if let Ok(mut cache) = self.cache.lock() {
                cache.put(texts[0].to_string(), out[0].clone());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider_for(server: &MockServer, dim: usize) -> HttpEmbeddingProvider {
        let url = url::Url::parse(&server.uri()).unwrap();
        HttpEmbeddingProvider::new(
            url,
            "test-model".into(),
            Some("k".into()),
            dim,
            Duration::from_secs(2),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn happy_path_returns_ordered_vectors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .and(header("authorization", "Bearer k"))
            .and(body_json(serde_json::json!({
                "model": "test-model",
                "input": ["hola", "mundo"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "embedding": [0.2, 0.4], "index": 1 },
                    { "embedding": [0.1, 0.3], "index": 0 }
                ]
            })))
            .mount(&server)
            .await;

        let p = provider_for(&server, 2);
        let out = p.embed(&["hola", "mundo"]).await.unwrap();
        assert_eq!(out, vec![vec![0.1, 0.3], vec![0.2, 0.4]]);
    }

    #[tokio::test]
    async fn dimension_mismatch_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": [0.1, 0.2, 0.3], "index": 0 }]
            })))
            .mount(&server)
            .await;
        let p = provider_for(&server, 2);
        let err = p.embed(&["hi"]).await.unwrap_err().to_string();
        assert!(err.contains("dimension mismatch"), "got: {err}");
    }

    #[tokio::test]
    async fn http_error_surfaced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(500).set_body_string("bad"))
            .mount(&server)
            .await;
        let p = provider_for(&server, 2);
        let err = p.embed(&["hi"]).await.unwrap_err().to_string();
        assert!(err.contains("500"), "got: {err}");
    }

    #[tokio::test]
    async fn single_query_cache_skips_second_http_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": [0.7, 0.8], "index": 0 }]
            })))
            .expect(1) // only one request expected
            .mount(&server)
            .await;
        let p = provider_for(&server, 2);
        let a = p.embed(&["hello"]).await.unwrap();
        let b = p.embed(&["hello"]).await.unwrap();
        assert_eq!(a, b);
    }
}
