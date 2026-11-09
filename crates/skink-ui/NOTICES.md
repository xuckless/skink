# Third-party notices: skink-ui

No code is copied into this crate; these are linked dependencies added for
previews (licences checked by cargo-deny).

| Crate | Licence | Use |
|---|---|---|
| [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) 0.13 | MIT | Markdown preview (`:preview`, Alt-V); default build, ~0.4 MB |
| [resvg](https://github.com/linebender/resvg) 0.49 | Apache-2.0 OR MIT | SVG preview, half-block raster; optional `svg-preview` feature (~0.8 MB, off by default: the default build would pass 15 MB) |
