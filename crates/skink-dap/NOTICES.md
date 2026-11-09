# Third-party notices for skink-dap

## Lapce (Apache-2.0), through skink-lsp

- Content-Length framing (`skink_lsp::transport`) is reused, not copied again; it is adapted from
  `lapce-proxy/src/plugin/lsp.rs` @ f66ffaa95387794205c35864517de70958b9ea8d (see
  crates/skink-lsp/NOTICES.md). The no-console-window flag in `src/client.rs` follows the same
  Lapce code.

## Debug Adapter Protocol schema (MIT)

- Project: https://github.com/microsoft/debug-adapter-protocol (`debugAdapterProtocol.json`)
- License: MIT, Copyright (c) Microsoft Corporation
- Used for: request, response and event names and field names in `src/types.rs`,
  `src/session.rs` and `src/fake.rs`. No code was copied; the types are written for Skink and
  hold only the fields it reads.

## Not used

- Lapce's `lapce-rpc/src/dap_types.rs` and Helix's `helix-dap` are MPL-2.0 (banned by
  `deny.toml`); neither was copied. The start-up order (initialize → launch → initialized →
  setBreakpoints → configurationDone) is the protocol's documented sequence.
