use agent_memory::{derive_concept_tags, LongTermMemory, MAX_CONCEPT_TAGS};

async fn open_temp_db() -> LongTermMemory {
    LongTermMemory::open(":memory:").await.unwrap()
}

#[tokio::test]
async fn remember_populates_concept_tags() {
    let db = open_temp_db().await;
    let agent = "c-agent";

    db.remember(agent, "OpenAI quota monitoring endpoint", &["ops"]).await.unwrap();
    let results = db.recall(agent, "quota", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].concept_tags.iter().any(|t| t == "openai"),
        "expected 'openai' tag, got {:?}",
        results[0].concept_tags
    );
}

#[tokio::test]
async fn recall_expands_query_via_glossary_tag() {
    let db = open_temp_db().await;
    let agent = "c-agent-2";

    // Stored content contains "OpenAI" which derives tag `openai`.
    db.remember(agent, "We use OpenAI for embedding workloads", &[]).await.unwrap();
    // Unrelated noise to ensure the match isn't trivial.
    db.remember(agent, "Cluster autoscaler pod limits", &[]).await.unwrap();

    // Query derives `openai` as a concept tag (glossary match). Raw
    // FTS5 MATCH of just "openai" also hits the first row, so expansion
    // is additive — the interesting case is the next test.
    let hits = db.recall(agent, "openai rate limits", 5).await.unwrap();
    assert!(hits.iter().any(|e| e.content.contains("OpenAI")));
}

#[tokio::test]
async fn recall_with_tags_matches_via_expansion() {
    let db = open_temp_db().await;
    let agent = "c-agent-3";

    db.remember(agent, "We use OpenAI for embedding workloads", &[]).await.unwrap();
    db.remember(agent, "Router VLAN segmentation on core switch", &[]).await.unwrap();

    // Query text deliberately does NOT contain "openai" or "router".
    // Caller-supplied tags trigger FTS expansion.
    let hits = db
        .recall_with_tags(agent, "totally unrelated text", &["openai".to_string()], 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("OpenAI"));
}

#[test]
fn max_concept_tags_honored() {
    let snippet = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu";
    let tags = derive_concept_tags("", snippet, MAX_CONCEPT_TAGS);
    assert!(tags.len() <= MAX_CONCEPT_TAGS);
}
