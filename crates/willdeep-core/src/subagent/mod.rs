//! Delegated workers: the trades the parent Agent may dispatch, the contract
//! it dispatches them with, and the runtime that holds them to it.
//!
//! Split by responsibility rather than by type, because these four halves
//! change for different reasons:
//!
//! - [`types`] — the dispatch contract (shells, write scopes, task packets).
//!   Pure data, depended on by everything else, depending on nothing.
//! - [`profiles`] — the static trade table: window tiers, payload caps and
//!   the relay's hosted job-prompt models. Edited when a trade is added or
//!   retuned.
//! - [`catalog`] — dispatch: circuit breaker, write-set approval, verifier
//!   safety gate, worktree policy, background lifecycle.
//! - [`runner`] — execution: the attempt loop, the isolated child agent and
//!   the verifier that decides done, with [`brief`] composing the opening
//!   message and [`audit`] spot-checking what a report-only run cited.
//!
//! The public path stays `willdeep_core::subagent::*`; everything callers
//! used before the split is re-exported here.

mod audit;
mod brief;
mod catalog;
mod profiles;
mod runner;
mod text;
mod types;

#[cfg(test)]
mod test_support;

pub use audit::{CitationAudit, audit_citations};
pub use catalog::{SubagentCatalog, TierBinding};
pub use profiles::{
    HOSTED_WORKER_MODEL_PREFIX, STANDARD_WINDOW, WORKER_WINDOW_BALANCED, WORKER_WINDOW_STANDARD,
    WORKER_WINDOW_WIDE, builtin_profiles, hosted_worker_model,
};
pub use types::{
    PUBLIC_SUBAGENT_IDS, SubagentProfile, SubagentShell, SubagentWriteScope, TaskPacket,
    TaskVerifier, public_profile_id,
};

pub(crate) use types::SpawnAgentArgs;
