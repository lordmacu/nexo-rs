#![cfg(feature = "duckduckgo")]

use agent_web_search::providers::duckduckgo::DuckDuckGoProvider;

const SAMPLE: &str = r#"
<html><body>
<div class="result">
  <h2 class="result__title">
    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa">First Title</a>
  </h2>
  <a class="result__snippet">First snippet text.</a>
</div>
<div class="result">
  <h2 class="result__title">
    <a class="result__a" href="https://example.org/b">Second Title</a>
  </h2>
  <a class="result__snippet">Second snippet text.</a>
</div>
</body></html>
"#;

#[test]
fn parses_two_results_decoding_uddg_redirect() {
    let hits = DuckDuckGoProvider::parse_html(SAMPLE, 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].url, "https://example.com/a");
    assert_eq!(hits[0].title, "First Title");
    assert_eq!(hits[0].snippet, "First snippet text.");
    assert_eq!(hits[1].url, "https://example.org/b");
}

#[test]
fn caps_at_max_hits() {
    let hits = DuckDuckGoProvider::parse_html(SAMPLE, 1).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn detects_bot_challenge() {
    let challenge = "<html><body><h1>Unusual Traffic</h1>If you keep getting blocked from sending requests please solve the captcha.</body></html>";
    let err = DuckDuckGoProvider::parse_html(challenge, 10).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("429"), "got {msg}");
}
