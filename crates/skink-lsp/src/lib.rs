//! Thin LSP client seam. Client plumbing (JSON-RPC transport, server
//! lifecycle) is lifted from Lapce (Apache-2.0) in Phase 2. A `Delegated`
//! provider uses LSP-backed MCP tools served by EveryAI instead.

use skink_core::Backend;

pub trait LspProvider {
    fn backend(&self) -> Backend;
}

/// Placeholder until the client lands.
pub struct NoLsp(pub Backend);

impl LspProvider for NoLsp {
    fn backend(&self) -> Backend {
        self.0
    }
}
