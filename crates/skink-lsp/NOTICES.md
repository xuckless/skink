# Third-party notices for skink-lsp

## Lapce (Apache-2.0)

- Project: https://github.com/lapce/lapce
- Commit: f66ffaa95387794205c35864517de70958b9ea8d
- License: Apache-2.0, Copyright (c) Lapce Authors
- Files adapted:
  - `lapce-proxy/src/plugin/lsp.rs` (`parse_header`, `read_message`, writer thread framing,
    `LspClient::process` creation flags) -> `src/transport.rs`, `src/client.rs`.
    Changes: byte-based body, explicit EOF error (upstream loops on EOF), tolerant of unknown
    headers / bare `\n`, 256 MiB frame cap, no anyhow/tracing/crossbeam, io::Error results.
  - `lapce-proxy/src/plugin/lsp.rs` (`LspClient::initialize`, `shutdown`) and
    `lapce-proxy/src/plugin/psp.rs` (`handle_plugin_server_message`, host request replies for
    `client/registerCapability`, `window/workDoneProgress/create`) -> `src/client.rs`
    (handshake, request id -> response routing, server->client request handling, shutdown/exit).
    Changes: Lapce's plugin catalog, WASI plugins, proxy dispatcher, GUI RPC and floem types
    dropped; single connection object; pending-request map with timeouts and `$/cancelRequest`;
    replies for workspace/configuration, workspaceFolders, applyEdit, showMessageRequest,
    showDocument and `*/refresh` added.
  - `lapce-proxy/src/plugin/mod.rs` (`client_capabilities`): used as a reference for which
    capabilities to advertise; Skink's list is a fresh, smaller JSON value
    (`client::client_capabilities`).

Apache-2.0 text: https://www.apache.org/licenses/LICENSE-2.0

## Ideas only

- helix-lsp (MPL-2.0): the "one server per (language, root), started lazily" registry shape.
  No code or queries were read or copied.

## Dependencies

- `serde`, `serde_json` (MIT OR Apache-2.0).
- `lsp-types` 0.97 (MIT) is a **dev-dependency only**: tests deserialize Skink's
  `initialize` params/capabilities into its types to prove they are spec-valid. Runtime
  parsing of server replies is hand-rolled and lenient (`src/wire.rs`) so the release
  binary does not carry lsp-types / fluent-uri.
