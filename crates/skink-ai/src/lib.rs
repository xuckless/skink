//! Agent (ACP) seam. Standalone, a `Local` controller spawns ACP agents
//! itself; inside EveryAI a `Delegated` controller hands that over.
//! Phase 0 defines the seam only.

use skink_core::Backend;

pub trait AgentController {
    fn backend(&self) -> Backend;
}

/// Placeholder until the ACP client lands (Phase 4/5).
pub struct NoAgents(pub Backend);

impl AgentController for NoAgents {
    fn backend(&self) -> Backend {
        self.0
    }
}
