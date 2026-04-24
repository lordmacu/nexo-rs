//! Exhaustive service catalog. New services land here.

mod infra;
mod llm;
mod memory;
pub mod anthropic_oauth;
pub mod minimax_oauth;
mod plugins;
mod runtime;
mod skills;

use crate::registry::ServiceDef;

/// Return every service the wizard knows about, in display order.
pub fn all() -> Vec<ServiceDef> {
    let mut v = Vec::new();
    v.extend(llm::defs());
    v.extend(memory::defs());
    v.extend(plugins::defs());
    v.extend(skills::defs());
    v.extend(infra::defs());
    v.extend(runtime::defs());
    v
}
