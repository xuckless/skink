# Third-party notices: skink-host

No third-party source code is copied into this crate.

## Written from scratch (with reasons)

* `src/http.rs`: a std-only HTTP/1.1 client (head parser, Content-Length,
  chunked and read-until-close bodies, SSE reader). The permissive options
  (`httparse`, `ureq`, `minreq`, `chunked_transfer`; MIT and/or Apache-2.0)
  are general purpose and bring unsafe SIMD parsing, TLS and redirect logic we
  do not need for one loopback POST per connection. The chunked decoder and
  head reader follow the same structure as `ureq`/`chunked_transfer` (idea
  only, no code), implemented against RFC 9112 and the WHATWG SSE spec.
* `src/client.rs`: MCP client written against the public MCP specification
  (https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

## Read-only reference (no code used)

* EveryAI `frontend/lib/features/code/host/host_server_io.dart`
  (xuckless/everyai-mono, FSL-1.1-ALv2): read only to learn the wire behaviour
  of the host we talk to (endpoint, Bearer token, exact `Host` header guard,
  protocol versions, SSE `tools/call` replies, `notifications/cancelled`).
  Nothing was copied.

## Dependencies

* `serde`, `serde_json` (MIT OR Apache-2.0).
