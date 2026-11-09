//! One call that picks the backends: EveryAI's when its host answers, the
//! standalone ones otherwise.

use crate::delegated::{DelegatedAgents, DelegatedLsp, HostLink, LinkPolicy, lsp_timeouts};
use crate::{FallbackLsp, McpClient, connect_from_vars};
use skink_ai::acp::{BufferSource, NoBuffers};
use skink_ai::local::Waker;
use skink_ai::{AgentController, LocalAgents};
use skink_core::Backend;
use skink_lsp::{LocalLsp, LspProvider, ServerRegistry};
use std::path::Path;
use std::sync::Arc;

/// The editor's language and agent backends plus which kind they are.
///
/// `agents` is a `Box`, not an `Arc`: [`AgentController`] takes `&mut self`
/// (the UI loop owns it); only the LSP side is shared across threads.
pub struct Providers {
    /// `Delegated` when EveryAI answered the handshake, else `Local`.
    pub backend: Backend,
    pub lsp: Arc<dyn LspProvider>,
    pub agents: Box<dyn AgentController>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

/// Optional wiring for [`providers_with`].
#[derive(Default, Clone)]
pub struct ProviderOptions {
    /// Called from background threads whenever an LSP or agent event is queued.
    pub waker: Option<Waker>,
    /// Open editor buffers, so *local* agents read unsaved text. Unused when
    /// delegated (the host gets buffers as `Doc.text` instead).
    pub buffers: Option<Arc<dyn BufferSource>>,
    /// Failure policy of the delegated link.
    pub policy: Option<LinkPolicy>,
    /// Languages for Skink's own servers (built-ins merged with settings);
    /// `None` means the built-in table.
    pub languages: Option<Vec<skink_lsp::Language>>,
}

impl Providers {
    /// Delegated providers over an already connected client. The LSP side gets
    /// its own fork with interactive deadlines ([`lsp_timeouts`]); the agent
    /// side uses `client` (each turn forks again), so cancelling an agent turn
    /// never cuts an LSP query.
    pub fn delegated(client: McpClient, opts: &ProviderOptions) -> Providers {
        let policy = opts.policy.unwrap_or_default();
        let lsp_link = Arc::new(HostLink::with_policy(
            client.fork_with_timeouts(lsp_timeouts()),
            policy,
        ));
        let agent_link = Arc::new(HostLink::with_policy(client, policy));
        let mut lsp = DelegatedLsp::new(lsp_link);
        let mut agents = DelegatedAgents::new(agent_link);
        if let Some(w) = &opts.waker {
            let w = Arc::clone(w);
            lsp = lsp.with_waker(Arc::clone(&w));
            agents = agents.with_waker(w);
        }
        Providers {
            backend: Backend::Delegated,
            lsp: Arc::new(lsp),
            agents: Box::new(agents),
        }
    }

    /// Skink's own language servers (no agents).
    pub fn local_lsp(root: &Path, opts: &ProviderOptions) -> Arc<dyn LspProvider> {
        let mut registry = ServerRegistry::with_languages(
            root,
            opts.languages
                .clone()
                .unwrap_or_else(skink_lsp::default_languages),
        );
        if let Some(w) = &opts.waker {
            let w = Arc::clone(w);
            registry.set_waker(move || w());
        }
        Arc::new(LocalLsp::with_registry(registry))
    }

    /// Standalone providers: real language servers and local ACP agents.
    pub fn local(root: &Path, opts: &ProviderOptions) -> Providers {
        let mut registry = ServerRegistry::with_languages(
            root,
            opts.languages
                .clone()
                .unwrap_or_else(skink_lsp::default_languages),
        );
        let mut agents = {
            let buffers = opts
                .buffers
                .clone()
                .unwrap_or_else(|| Arc::new(NoBuffers) as Arc<dyn BufferSource>);
            LocalAgents::for_workspace(root, buffers)
        };
        if let Some(w) = &opts.waker {
            let for_lsp = Arc::clone(w);
            registry.set_waker(move || for_lsp());
            agents = agents.with_waker(Arc::clone(w));
        }
        Providers {
            backend: Backend::Local,
            lsp: Arc::new(LocalLsp::with_registry(registry)),
            agents: Box::new(agents),
        }
    }
}

/// [`providers_with`] on the process environment (`SKINK_HOST_URL` /
/// `SKINK_HOST_TOKEN`) with default options.
pub fn providers_from_env(root: &Path) -> Providers {
    providers_with(root, |k| std::env::var(k).ok(), &ProviderOptions::default())
}

/// Connect to the host described by `get` (short start-up probe, see
/// [`connect_from_vars`]); delegated providers if it answers, local ones
/// (rooted at `root`) in every other case. Never fails and never panics.
pub fn providers_with(
    root: &Path,
    get: impl Fn(&str) -> Option<String>,
    opts: &ProviderOptions,
) -> Providers {
    match connect_from_vars(get) {
        Some(client) => {
            // Files the host has no server for use Skink's own (per file).
            let mut p = Providers::delegated(client, opts);
            let local = Providers::local_lsp(root, opts);
            p.lsp = Arc::new(FallbackLsp::new(p.lsp, local));
            p
        }
        None => Providers::local(root, opts),
    }
}
