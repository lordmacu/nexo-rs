//! SSE parser benchmarks for the three streaming providers we own:
//! OpenAI (also covers OpenAI-compat: minimax, deepseek, llama.cpp,
//! mistral.rs, ollama, vllm, etc.), Anthropic, Gemini.
//!
//! Each fixture streams ~50 chunks (typical short answer) and the
//! bench measures end-to-end parse-to-`StreamChunk` throughput.
//!
//! Run with:
//!     cargo bench -p nexo-llm --bench sse_parsers

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use futures::stream;
use futures::StreamExt;
use nexo_llm::stream::{parse_anthropic_sse, parse_gemini_sse, parse_openai_sse};

fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// 50 OpenAI-style `chat.completion.chunk` events with one-token deltas.
fn openai_fixture() -> Vec<Bytes> {
    let mut chunks: Vec<Bytes> = (0..50)
        .map(|i| {
            let json = format!(
                r#"{{"id":"chatcmpl-{i}","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{{"index":0,"delta":{{"content":"tok{i} "}},"finish_reason":null}}]}}"#
            );
            Bytes::from(format!("data: {json}\n\n"))
        })
        .collect();
    chunks.push(Bytes::from_static(b"data: [DONE]\n\n"));
    chunks
}

/// 50 Anthropic `content_block_delta` events. Anthropic uses an
/// explicit `event:` prefix per SSE record so the parser path
/// differs from OpenAI's.
fn anthropic_fixture() -> Vec<Bytes> {
    let mut out = vec![
        Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"claude-3-5-sonnet\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
        ),
        Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        ),
    ];
    for i in 0..50 {
        let json = format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"tok{i} "}}}}"#
        );
        out.push(Bytes::from(format!(
            "event: content_block_delta\ndata: {json}\n\n"
        )));
    }
    out.push(Bytes::from_static(
        b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    ));
    out.push(Bytes::from_static(
        b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ));
    out
}

/// 50 Gemini `streamGenerateContent` chunks. Each is a complete JSON
/// object (Gemini doesn't use SSE `event:` framing — it's
/// newline-delimited JSON inside a `data:` SSE wrapper).
fn gemini_fixture() -> Vec<Bytes> {
    (0..50)
        .map(|i| {
            let json = format!(
                r#"{{"candidates":[{{"content":{{"parts":[{{"text":"tok{i} "}}],"role":"model"}},"index":0}}]}}"#
            );
            Bytes::from(format!("data: {json}\n\n"))
        })
        .collect()
}

fn bench_openai(c: &mut Criterion) {
    let runtime = make_runtime();
    let fixture = openai_fixture();
    let n = fixture.len() as u64;

    let mut group = c.benchmark_group("sse_parsers/openai");
    group.throughput(Throughput::Elements(n));
    group.bench_function("50_text_deltas", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let owned: Vec<Bytes> = fixture.clone();
                let s = stream::iter(owned.into_iter().map(Ok::<_, std::io::Error>));
                let mut out = parse_openai_sse(s);
                let mut count = 0usize;
                while let Some(chunk) = out.next().await {
                    black_box(chunk.ok());
                    count += 1;
                }
                black_box(count)
            })
        });
    });
    group.finish();
}

fn bench_anthropic(c: &mut Criterion) {
    let runtime = make_runtime();
    let fixture = anthropic_fixture();
    let n = fixture.len() as u64;

    let mut group = c.benchmark_group("sse_parsers/anthropic");
    group.throughput(Throughput::Elements(n));
    group.bench_function("50_text_deltas", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let owned: Vec<Bytes> = fixture.clone();
                let s = stream::iter(owned.into_iter().map(Ok::<_, std::io::Error>));
                let mut out = parse_anthropic_sse(s);
                let mut count = 0usize;
                while let Some(chunk) = out.next().await {
                    black_box(chunk.ok());
                    count += 1;
                }
                black_box(count)
            })
        });
    });
    group.finish();
}

fn bench_gemini(c: &mut Criterion) {
    let runtime = make_runtime();
    let fixture = gemini_fixture();
    let n = fixture.len() as u64;

    let mut group = c.benchmark_group("sse_parsers/gemini");
    group.throughput(Throughput::Elements(n));
    group.bench_function("50_text_deltas", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let owned: Vec<Bytes> = fixture.clone();
                let s = stream::iter(owned.into_iter().map(Ok::<_, std::io::Error>));
                let mut out = parse_gemini_sse(s);
                let mut count = 0usize;
                while let Some(chunk) = out.next().await {
                    black_box(chunk.ok());
                    count += 1;
                }
                black_box(count)
            })
        });
    });
    group.finish();
}

criterion_group!(benches, bench_openai, bench_anthropic, bench_gemini);
criterion_main!(benches);
