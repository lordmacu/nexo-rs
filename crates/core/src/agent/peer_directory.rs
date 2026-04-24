//! Peer directory — list of other agents in the same process.
//!
//! Built at boot from every `AgentConfig` and handed to each agent's
//! runtime via `AgentContext::with_peers`. `llm_behavior` renders it
//! as a `# PEERS` block after the workspace section so the LLM knows
//! who it can delegate to and what role each peer plays.

use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PeerSummary {
    pub id: String,
    pub description: String,
}

pub struct PeerDirectory {
    peers: Vec<PeerSummary>,
}

impl PeerDirectory {
    pub fn new(peers: Vec<PeerSummary>) -> Arc<Self> {
        Arc::new(Self { peers })
    }

    /// Render the peers block as seen by `self_id`. Excludes the agent
    /// itself and — if `allowed_delegates` is populated — annotates
    /// whether each peer is reachable. Returns `None` when there are
    /// no peers (so llm_behavior can skip emitting an empty block).
    ///
    /// `allowed_delegates` uses the same trailing-`*` glob semantics as
    /// `DelegationTool`. Empty list = every peer reachable (back-compat).
    pub fn render_for(&self, self_id: &str, allowed_delegates: &[String]) -> Option<String> {
        let others: Vec<&PeerSummary> =
            self.peers.iter().filter(|p| p.id != self_id).collect();
        if others.is_empty() {
            return None;
        }
        let mut out = String::from("# PEERS\n");
        out.push_str(
            "Other agents you can reach via `delegate({agent_id, task, ...})`:\n\n",
        );
        for p in others {
            let reachable = allowed_delegates.is_empty()
                || allowed_delegates
                    .iter()
                    .any(|pat| match pat.strip_suffix('*') {
                        Some(stem) => p.id.starts_with(stem),
                        None => pat == &p.id,
                    });
            let mark = if reachable { "✓" } else { "✗" };
            if p.description.is_empty() {
                out.push_str(&format!("- {mark} `{}`\n", p.id));
            } else {
                out.push_str(&format!(
                    "- {mark} `{}` — {}\n",
                    p.id,
                    p.description.trim()
                ));
            }
        }
        Some(out)
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, desc: &str) -> PeerSummary {
        PeerSummary { id: id.into(), description: desc.into() }
    }

    #[test]
    fn renders_others_with_reachability_marks() {
        let dir = PeerDirectory::new(vec![
            peer("boss", "decides strategy"),
            peer("ventas", "handles sales"),
            peer("soporte_lvl1", "first-line support"),
        ]);
        // From boss's perspective with allowlist [ventas, soporte_*].
        let block = dir
            .render_for("boss", &["ventas".into(), "soporte_*".into()])
            .expect("block exists");
        assert!(block.contains("# PEERS"));
        assert!(!block.contains("`boss`"), "self must be filtered out");
        assert!(block.contains("✓ `ventas`"));
        assert!(block.contains("✓ `soporte_lvl1`"));
        // No peer falls off the list even if unreachable — just marked ✗.
    }

    #[test]
    fn empty_allowlist_marks_everything_reachable() {
        let dir = PeerDirectory::new(vec![
            peer("a", ""),
            peer("b", "desc b"),
        ]);
        let block = dir.render_for("a", &[]).expect("block");
        assert!(block.contains("✓ `b`"));
        assert!(block.contains("desc b"));
    }

    #[test]
    fn unreachable_peers_marked() {
        let dir = PeerDirectory::new(vec![
            peer("a", ""),
            peer("b", ""),
            peer("c", ""),
        ]);
        let block = dir.render_for("a", &["b".into()]).expect("block");
        assert!(block.contains("✓ `b`"));
        assert!(block.contains("✗ `c`"));
    }

    #[test]
    fn single_agent_has_no_peers_block() {
        let dir = PeerDirectory::new(vec![peer("alone", "")]);
        assert!(dir.render_for("alone", &[]).is_none());
    }
}
