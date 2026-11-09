//! LSP JSON <-> Skink types. Parsing is deliberately lenient (unknown fields
//! ignored, `null` everywhere accepted) and hand-rolled instead of going
//! through `lsp-types`, which keeps the dependency out of the release build
//! and tolerates the quirks real servers have. `lsp-types` is used in tests
//! to check that what we *send* is valid.
//!
//! Positions in server messages are UTF-16; every conversion goes through a
//! [`Resolver`] that finds the line text (open document, else disk, else
//! assume ASCII).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    doc::Doc,
    pos::utf16_to_char,
    types::*,
    uri::{uri_key, uri_to_path},
};

pub(crate) type Docs = Mutex<HashMap<String, Doc>>;

/// Column conversion against the open documents, falling back to the file on
/// disk (read at most once per resolver) and finally to the identity. The
/// documents lock is taken per lookup, never across a disk read, so a slow
/// file system cannot stall `did_change` on another thread.
pub(crate) struct Resolver<'a> {
    docs: &'a Docs,
    disk: HashMap<String, Option<Doc>>,
}

impl<'a> Resolver<'a> {
    pub fn new(docs: &'a Docs) -> Self {
        Self {
            docs,
            disk: HashMap::new(),
        }
    }

    fn with_doc<T>(&mut self, uri: &str, f: impl FnOnce(&Doc) -> T) -> Option<T> {
        let key = uri_key(uri);
        {
            let docs = self.docs.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(d) = docs.get(&key) {
                return Some(f(d));
            }
        }
        self.disk
            .entry(key)
            .or_insert_with(|| uri_to_path(uri).and_then(|p| Doc::from_disk(&p)))
            .as_ref()
            .map(f)
    }

    pub fn pos(&mut self, uri: &str, p: &WPos) -> Position {
        let col = self
            .with_doc(uri, |d| d.char_col(p.line, p.character))
            .unwrap_or(p.character as usize);
        Position::new(p.line as usize, col)
    }

    pub fn range(&mut self, uri: &str, r: &WRange) -> Range {
        Range::new(self.pos(uri, &r.start), self.pos(uri, &r.end))
    }

    /// Skink position -> LSP `{line, character}`.
    pub fn lsp_pos(&mut self, uri: &str, p: Position) -> Value {
        let col = self
            .with_doc(uri, |d| d.utf16_col(p.line, p.col))
            .unwrap_or(p.col as u32);
        json!({"line": p.line, "character": col})
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WPos {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WRange {
    pub start: WPos,
    pub end: WPos,
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value, what: &str) -> Result<T> {
    serde_json::from_value(v).map_err(|e| LspError::Protocol(format!("bad {what}: {e}")))
}

fn parse_opt<T: for<'de> Deserialize<'de>>(v: Value, what: &str) -> Result<Option<T>> {
    if v.is_null() {
        Ok(None)
    } else {
        parse(v, what).map(Some)
    }
}

// ---- hover -----------------------------------------------------------------

/// `MarkedString | MarkedString[] | MarkupContent` -> text.
fn marked_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(marked_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(o) => {
            let value = o.get("value").and_then(Value::as_str).unwrap_or_default();
            match o.get("language").and_then(Value::as_str) {
                Some(lang) if !value.is_empty() => format!("```{lang}\n{value}\n```"),
                _ => value.to_string(),
            }
        }
        _ => String::new(),
    }
}

pub(crate) fn hover(res: &mut Resolver, uri: &str, v: Value) -> Result<Option<Hover>> {
    #[derive(Deserialize)]
    struct W {
        contents: Value,
        range: Option<WRange>,
    }
    let Some(w) = parse_opt::<W>(v, "hover")? else {
        return Ok(None);
    };
    let text = marked_text(&w.contents);
    Ok((!text.trim().is_empty()).then(|| Hover {
        text,
        range: w.range.map(|r| res.range(uri, &r)),
    }))
}

// ---- locations ---------------------------------------------------------------

pub(crate) fn locations(res: &mut Resolver, v: Value) -> Result<Vec<Location>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct W {
        uri: Option<String>,
        range: Option<WRange>,
        target_uri: Option<String>,
        target_range: Option<WRange>,
        target_selection_range: Option<WRange>,
    }
    let items = match v {
        Value::Null => return Ok(vec![]),
        Value::Array(a) => a,
        one => vec![one],
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let w: W = parse(it, "location")?;
        let (uri, range) = match (w.uri, w.target_uri) {
            (Some(u), _) => (u, w.range),
            (None, Some(u)) => (u, w.target_selection_range.or(w.target_range)),
            _ => continue,
        };
        // Non-file URIs (e.g. `jar:`, `deno:`) cannot be opened as files.
        let (Some(path), Some(range)) = (uri_to_path(&uri), range) else {
            continue;
        };
        out.push(Location {
            path,
            range: res.range(&uri, &range),
        });
    }
    Ok(out)
}

// ---- edits -------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WEdit {
    range: Option<WRange>,
    insert: Option<WRange>,
    new_text: String,
}

fn conv_edit(res: &mut Resolver, uri: &str, e: WEdit) -> Option<TextEdit> {
    // InsertReplaceEdit has `insert`/`replace` instead of `range`; prefer insert.
    let r = e.range.or(e.insert)?;
    Some(TextEdit {
        range: res.range(uri, &r),
        new_text: e.new_text,
    })
}

pub(crate) fn text_edits(res: &mut Resolver, uri: &str, v: Value) -> Result<Vec<TextEdit>> {
    let Some(list) = parse_opt::<Vec<WEdit>>(v, "text edits")? else {
        return Ok(vec![]);
    };
    Ok(list
        .into_iter()
        .filter_map(|e| conv_edit(res, uri, e))
        .collect())
}

pub(crate) fn workspace_edit(res: &mut Resolver, v: Value) -> Result<Option<WorkspaceEdit>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct W {
        changes: Option<HashMap<String, Vec<WEdit>>>,
        document_changes: Option<Vec<Value>>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Op {
        kind: Option<String>,
        uri: Option<String>,
        old_uri: Option<String>,
        new_uri: Option<String>,
        text_document: Option<TdId>,
        edits: Option<Vec<WEdit>>,
    }
    #[derive(Deserialize)]
    struct TdId {
        uri: String,
    }
    let Some(w) = parse_opt::<W>(v, "workspace edit")? else {
        return Ok(None);
    };
    let mut out = WorkspaceEdit::default();
    let add = |res: &mut Resolver, uri: &str, edits: Vec<WEdit>, out: &mut WorkspaceEdit| {
        let Some(path) = uri_to_path(uri) else { return };
        let edits = edits
            .into_iter()
            .filter_map(|e| conv_edit(res, uri, e))
            .collect();
        // Merge entries for the same file, preserving order.
        match out.files.iter_mut().find(|f| f.path == path) {
            Some(f) => f.edits.extend::<Vec<TextEdit>>(edits),
            None => out.files.push(FileEdit { path, edits }),
        }
    };
    if let Some(dc) = w.document_changes {
        for item in dc {
            let op: Op = parse(item, "document change")?;
            match (op.kind.as_deref(), op.text_document, op.edits) {
                (None, Some(td), Some(edits)) => add(res, &td.uri, edits, &mut out),
                (Some("create"), ..) => {
                    if let Some(path) = op.uri.as_deref().and_then(uri_to_path) {
                        out.ops.push(FileOp::Create { path });
                    }
                }
                (Some("delete"), ..) => {
                    if let Some(path) = op.uri.as_deref().and_then(uri_to_path) {
                        out.ops.push(FileOp::Delete { path });
                    }
                }
                (Some("rename"), ..) => {
                    if let (Some(from), Some(to)) = (
                        op.old_uri.as_deref().and_then(uri_to_path),
                        op.new_uri.as_deref().and_then(uri_to_path),
                    ) {
                        out.ops.push(FileOp::Rename { from, to });
                    }
                }
                _ => {}
            }
        }
    } else if let Some(changes) = w.changes {
        let mut entries: Vec<_> = changes.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0)); // stable order for callers/tests
        for (uri, edits) in entries {
            add(res, &uri, edits, &mut out);
        }
    }
    Ok(Some(out))
}

// ---- completion ----------------------------------------------------------------

pub(crate) fn completion(res: &mut Resolver, uri: &str, v: Value) -> Result<Vec<CompletionItem>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct W {
        label: String,
        kind: Option<u32>,
        detail: Option<String>,
        insert_text: Option<String>,
        insert_text_format: Option<u32>,
        text_edit: Option<WEdit>,
        additional_text_edits: Option<Vec<WEdit>>,
        sort_text: Option<String>,
        documentation: Option<Value>,
    }
    let items = match v {
        Value::Null => return Ok(vec![]),
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("items") {
            Some(Value::Array(a)) => a,
            _ => return Ok(vec![]),
        },
        _ => return Err(LspError::Protocol("bad completion result".into())),
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        // Items without documentation may get it from completionItem/resolve.
        let raw = it.get("documentation").is_none().then(|| it.clone());
        let w: W = parse(it, "completion item")?;
        let edit = w.text_edit.and_then(|e| conv_edit(res, uri, e));
        let insert_text = edit
            .as_ref()
            .map(|e| e.new_text.clone())
            .or(w.insert_text)
            .unwrap_or_else(|| w.label.clone());
        out.push(CompletionItem {
            label: w.label,
            insert_text,
            snippet: w.insert_text_format == Some(2),
            documentation: w
                .documentation
                .as_ref()
                .map(marked_text)
                .filter(|d| !d.is_empty()),
            kind: w.kind.map_or(ItemKind::Other, ItemKind::from_completion),
            detail: w.detail,
            sort_text: w.sort_text,
            edit,
            additional_edits: w
                .additional_text_edits
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| conv_edit(res, uri, e))
                .collect(),
            resolve: raw.map_or(Resolve::Complete, Resolve::Available),
        });
    }
    Ok(out)
}

/// Documentation from a `completionItem/resolve` result.
pub(crate) fn resolved_documentation(v: &Value) -> Option<String> {
    v.get("documentation")
        .map(marked_text)
        .filter(|d| !d.is_empty())
}

// ---- workspace symbols, code actions, inlay hints ---------------------------------

pub(crate) fn workspace_symbols(res: &mut Resolver, v: Value) -> Result<Vec<SymbolInfo>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct W {
        name: String,
        kind: u32,
        container_name: Option<String>,
        location: WLoc,
    }
    #[derive(Deserialize)]
    struct WLoc {
        uri: String,
        range: Option<WRange>,
    }
    let Some(list) = parse_opt::<Vec<W>>(v, "workspace symbols")? else {
        return Ok(vec![]);
    };
    Ok(list
        .into_iter()
        .filter_map(|w| {
            let path = uri_to_path(&w.location.uri)?;
            let range = w
                .location
                .range
                .as_ref()
                .map(|r| res.range(&w.location.uri, r))
                .unwrap_or_default();
            Some(SymbolInfo {
                name: w.name,
                kind: ItemKind::from_symbol(w.kind),
                container: w.container_name.filter(|c| !c.is_empty()),
                location: Location { path, range },
            })
        })
        .collect())
}

fn server_command(v: &Value) -> Option<ServerCommand> {
    Some(ServerCommand {
        title: v.get("title")?.as_str()?.to_owned(),
        command: v.get("command")?.as_str()?.to_owned(),
        arguments: v
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

pub(crate) fn code_actions(res: &mut Resolver, v: Value) -> Result<Vec<CodeAction>> {
    let Value::Array(items) = v else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for it in items {
        // A bare `Command` has a string `command`; a `CodeAction` has a title
        // and optionally an `edit` and/or a nested command object.
        if it.get("command").is_some_and(Value::is_string) {
            if let Some(c) = server_command(&it) {
                out.push(CodeAction {
                    title: c.title.clone(),
                    kind: None,
                    preferred: false,
                    edit: None,
                    command: Some(c),
                });
            }
            continue;
        }
        let Some(title) = it.get("title").and_then(Value::as_str) else {
            continue;
        };
        if it.get("disabled").is_some() {
            continue;
        }
        let edit = match it.get("edit") {
            Some(e) => workspace_edit(res, e.clone())?,
            None => None,
        };
        out.push(CodeAction {
            title: title.to_owned(),
            kind: it.get("kind").and_then(Value::as_str).map(str::to_owned),
            preferred: it.get("isPreferred").and_then(Value::as_bool) == Some(true),
            edit,
            command: it.get("command").and_then(server_command),
        });
    }
    Ok(out)
}

pub(crate) fn inlay_hints(res: &mut Resolver, uri: &str, v: Value) -> Result<Vec<InlayHint>> {
    let Value::Array(items) = v else {
        return Ok(vec![]);
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let Some(pos) = it.get("position").cloned() else {
            continue;
        };
        let p: WPos = parse(pos, "inlay hint position")?;
        let label = match it.get("label") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("value").and_then(Value::as_str))
                .collect(),
            _ => continue,
        };
        out.push(InlayHint {
            position: res.pos(uri, &p),
            label,
            kind: match it.get("kind").and_then(Value::as_u64) {
                Some(1) => InlayKind::Type,
                Some(2) => InlayKind::Parameter,
                _ => InlayKind::Other,
            },
            pad_left: it.get("paddingLeft").and_then(Value::as_bool) == Some(true),
            pad_right: it.get("paddingRight").and_then(Value::as_bool) == Some(true),
        });
    }
    Ok(out)
}

// ---- signature help ------------------------------------------------------------

/// Characters `[a, b)` counted in UTF-16 units.
fn utf16_slice(s: &str, a: u32, b: u32) -> String {
    let (ca, cb) = (utf16_to_char(s, a), utf16_to_char(s, b));
    s.chars().skip(ca).take(cb.saturating_sub(ca)).collect()
}

pub(crate) fn signature_help(v: Value) -> Result<Option<SignatureHelp>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sig {
        label: String,
        documentation: Option<Value>,
        parameters: Option<Vec<Param>>,
        active_parameter: Option<u32>,
    }
    #[derive(Deserialize)]
    struct Param {
        label: Value,
        documentation: Option<Value>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct W {
        signatures: Vec<Sig>,
        active_signature: Option<u32>,
        active_parameter: Option<u32>,
    }
    let Some(w) = parse_opt::<W>(v, "signature help")? else {
        return Ok(None);
    };
    if w.signatures.is_empty() {
        return Ok(None);
    }
    let signatures = w
        .signatures
        .into_iter()
        .map(|s| {
            let parameters = s
                .parameters
                .unwrap_or_default()
                .into_iter()
                .map(|p| Parameter {
                    label: match &p.label {
                        Value::String(l) => l.clone(),
                        Value::Array(a) => match (
                            a.first().and_then(Value::as_u64),
                            a.get(1).and_then(Value::as_u64),
                        ) {
                            (Some(x), Some(y)) => utf16_slice(&s.label, x as u32, y as u32),
                            _ => String::new(),
                        },
                        _ => String::new(),
                    },
                    documentation: p
                        .documentation
                        .as_ref()
                        .map(marked_text)
                        .filter(|t| !t.is_empty()),
                })
                .collect();
            Signature {
                label: s.label,
                documentation: s
                    .documentation
                    .as_ref()
                    .map(marked_text)
                    .filter(|t| !t.is_empty()),
                parameters,
                active_parameter: s.active_parameter.map(|n| n as usize),
            }
        })
        .collect::<Vec<_>>();
    let active_signature = (w.active_signature.unwrap_or(0) as usize).min(signatures.len() - 1);
    Ok(Some(SignatureHelp {
        signatures,
        active_signature,
        active_parameter: w.active_parameter.map(|n| n as usize),
    }))
}

// ---- document symbols ------------------------------------------------------------

pub(crate) fn document_symbols(
    res: &mut Resolver,
    uri: &str,
    v: Value,
) -> Result<Vec<DocumentSymbol>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Nested {
        name: String,
        detail: Option<String>,
        kind: u32,
        range: WRange,
        selection_range: WRange,
        children: Option<Vec<Nested>>,
    }
    #[derive(Deserialize)]
    struct Flat {
        name: String,
        kind: u32,
        location: FlatLoc,
    }
    #[derive(Deserialize)]
    struct FlatLoc {
        uri: String,
        range: WRange,
    }
    fn nested(res: &mut Resolver, uri: &str, n: Nested) -> DocumentSymbol {
        DocumentSymbol {
            name: n.name,
            detail: n.detail,
            kind: ItemKind::from_symbol(n.kind),
            range: res.range(uri, &n.range),
            selection_range: res.range(uri, &n.selection_range),
            children: n
                .children
                .unwrap_or_default()
                .into_iter()
                .map(|c| nested(res, uri, c))
                .collect(),
        }
    }
    let items = match v {
        Value::Null => return Ok(vec![]),
        Value::Array(a) => a,
        _ => return Err(LspError::Protocol("bad documentSymbol result".into())),
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        if it.get("location").is_some() {
            let f: Flat = parse(it, "symbol information")?;
            let r = res.range(&f.location.uri, &f.location.range);
            out.push(DocumentSymbol {
                name: f.name,
                detail: None,
                kind: ItemKind::from_symbol(f.kind),
                range: r,
                selection_range: r,
                children: vec![],
            });
        } else {
            out.push(nested(res, uri, parse(it, "document symbol")?));
        }
    }
    Ok(out)
}

// ---- diagnostics -------------------------------------------------------------------

pub(crate) struct Published {
    pub uri: String,
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) fn publish_diagnostics(res: &mut Resolver, params: Value) -> Result<Published> {
    #[derive(Deserialize)]
    struct D {
        range: WRange,
        severity: Option<u32>,
        code: Option<Value>,
        source: Option<String>,
        message: String,
    }
    #[derive(Deserialize)]
    struct W {
        uri: String,
        version: Option<i32>,
        diagnostics: Vec<D>,
    }
    let w: W = parse(params, "publishDiagnostics")?;
    let diagnostics = w
        .diagnostics
        .into_iter()
        .map(|d| Diagnostic {
            range: res.range(&w.uri, &d.range),
            severity: match d.severity {
                Some(2) => Severity::Warning,
                Some(3) => Severity::Info,
                Some(4) => Severity::Hint,
                _ => Severity::Error,
            },
            message: d.message,
            source: d.source,
            code: d.code.and_then(|c| match c {
                Value::String(s) => Some(s),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }),
        })
        .collect();
    Ok(Published {
        uri: w.uri,
        version: w.version,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn docs_with(uri: &str, text: &str) -> Docs {
        Mutex::new(HashMap::from([(uri_key(uri), Doc::new(1, text.into()))]))
    }

    #[test]
    fn code_actions_commands_and_snippet_completions() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        let acts = code_actions(
            &mut r,
            json!([
                {"title": "Run", "command": "rust-analyzer.run", "arguments": [1]},
                {"title": "Fix", "kind": "quickfix", "isPreferred": true,
                 "edit": {"changes": {"file:///t/a.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": "x"}]}}},
                {"title": "Nope", "disabled": {"reason": "no"}}
            ]),
        )
        .unwrap();
        assert_eq!(acts.len(), 2);
        assert_eq!(
            acts[0].command.as_ref().unwrap().command,
            "rust-analyzer.run"
        );
        assert!(acts[1].preferred && acts[1].edit.as_ref().unwrap().files.len() == 1);
        let items = completion(
            &mut r,
            "file:///t/a.rs",
            json!([{"label": "push", "insertText": "push(${1:value})", "insertTextFormat": 2,
                    "documentation": {"kind": "markdown", "value": "Appends"}}]),
        )
        .unwrap();
        assert!(items[0].snippet);
        assert_eq!(items[0].documentation.as_deref(), Some("Appends"));
    }

    #[test]
    fn hover_variants() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        let h = hover(
            &mut r,
            "file:///x",
            json!({"contents": {"kind":"markdown","value":"**hi**"}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(h.text, "**hi**");
        let h = hover(
            &mut r,
            "file:///x",
            json!({"contents": ["a", {"language":"rust","value":"fn f()"}]}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(h.text, "a\n\n```rust\nfn f()\n```");
        assert!(hover(&mut r, "file:///x", Value::Null).unwrap().is_none());
        assert!(
            hover(&mut r, "file:///x", json!({"contents": ""}))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn hover_range_is_converted_to_chars() {
        let uri = "file:///tmp/a.rs";
        let docs = docs_with(uri, "a😀bc");
        let mut r = Resolver::new(&docs);
        let h = hover(&mut r, uri, json!({"contents":"x","range":{"start":{"line":0,"character":3},"end":{"line":0,"character":5}}}))
            .unwrap()
            .unwrap();
        assert_eq!(
            h.range.unwrap(),
            Range::new(Position::new(0, 2), Position::new(0, 4))
        );
    }

    #[test]
    fn locations_scalar_array_link_and_foreign_scheme() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        let rng = json!({"start":{"line":1,"character":2},"end":{"line":1,"character":4}});
        let one = locations(&mut r, json!({"uri":"file:///a/b.rs","range":rng})).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].line(), 1);
        assert_eq!(one[0].col(), 2);
        let many = locations(
            &mut r,
            json!([
                {"uri":"file:///a/b.rs","range":rng},
                {"uri":"jar:file:///x.jar!/A.class","range":rng},
                {"targetUri":"file:///c.rs","targetRange":rng,"targetSelectionRange":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}}
            ]),
        )
        .unwrap();
        assert_eq!(many.len(), 2);
        assert_eq!(many[1].path, uri_to_path("file:///c.rs").unwrap());
        assert_eq!(many[1].line(), 9);
        assert!(locations(&mut r, Value::Null).unwrap().is_empty());
    }

    #[test]
    fn completion_forms() {
        let uri = "file:///t.rs";
        let docs = docs_with(uri, "let s = Str");
        let mut r = Resolver::new(&docs);
        let edit = json!({"range":{"start":{"line":0,"character":8},"end":{"line":0,"character":11}},"newText":"String"});
        let v = json!({"isIncomplete":false,"items":[
            {"label":"String","kind":22,"detail":"struct","textEdit":edit,"additionalTextEdits":[edit]},
            {"label":"plain"},
            {"label":"ins","insertText":"ins()","kind":3},
            {"label":"ir","textEdit":{"newText":"ir","insert":{"start":{"line":0,"character":8},"end":{"line":0,"character":9}},"replace":{"start":{"line":0,"character":8},"end":{"line":0,"character":11}}}}
        ]});
        let items = completion(&mut r, uri, v).unwrap();
        assert_eq!(items[0].insert_text, "String");
        assert_eq!(items[0].kind, ItemKind::Struct);
        assert_eq!(
            items[0].edit.as_ref().unwrap().range.end,
            Position::new(0, 11)
        );
        assert_eq!(items[0].additional_edits.len(), 1);
        assert_eq!(items[1].insert_text, "plain");
        assert_eq!(items[1].kind, ItemKind::Other);
        assert_eq!(items[2].insert_text, "ins()");
        assert_eq!(
            items[3].edit.as_ref().unwrap().range.end,
            Position::new(0, 9)
        );
        assert_eq!(
            completion(&mut r, uri, json!([{"label":"a"}]))
                .unwrap()
                .len(),
            1
        );
        assert!(completion(&mut r, uri, Value::Null).unwrap().is_empty());
    }

    #[test]
    fn workspace_edit_changes_and_document_changes() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        let e = |n: &str| json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":n});
        let a = workspace_edit(
            &mut r,
            json!({"changes":{"file:///b.rs":[e("1")],"file:///a.rs":[e("2"),e("3")]}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.files.len(), 2);
        assert_eq!(a.files[0].path, PathBuf::from("/a.rs"));
        assert_eq!(a.files[0].edits.len(), 2);
        let b = workspace_edit(
            &mut r,
            json!({"documentChanges":[
                {"kind":"create","uri":"file:///new.rs"},
                {"textDocument":{"uri":"file:///a.rs","version":3},"edits":[e("x")]},
                {"kind":"rename","oldUri":"file:///a.rs","newUri":"file:///c.rs"},
                {"kind":"delete","uri":"file:///old.rs"}
            ]}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(b.files.len(), 1);
        assert_eq!(b.ops.len(), 3);
        assert_eq!(
            b.ops[1],
            FileOp::Rename {
                from: "/a.rs".into(),
                to: "/c.rs".into()
            }
        );
        assert!(workspace_edit(&mut r, Value::Null).unwrap().is_none());
    }

    #[test]
    fn signature_help_offsets_and_clamp() {
        let v = json!({"signatures":[{"label":"fn f(a: i32, b: 😀)","parameters":[{"label":[5,11]},{"label":"b: 😀","documentation":{"kind":"markdown","value":"doc"}}]}],"activeSignature":7,"activeParameter":1});
        let h = signature_help(v).unwrap().unwrap();
        assert_eq!(h.active_signature, 0);
        assert_eq!(h.active_parameter, Some(1));
        assert_eq!(h.signatures[0].parameters[0].label, "a: i32");
        assert_eq!(
            h.signatures[0].parameters[1].documentation.as_deref(),
            Some("doc")
        );
        assert!(signature_help(json!({"signatures":[]})).unwrap().is_none());
        assert!(signature_help(Value::Null).unwrap().is_none());
    }

    #[test]
    fn symbols_nested_and_flat() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        let rg = json!({"start":{"line":0,"character":0},"end":{"line":3,"character":1}});
        let nested = json!([{"name":"S","kind":23,"range":rg,"selectionRange":rg,"children":[{"name":"f","kind":8,"range":rg,"selectionRange":rg}]}]);
        let s = document_symbols(&mut r, "file:///a.rs", nested).unwrap();
        assert_eq!(s[0].kind, ItemKind::Struct);
        assert_eq!(s[0].children[0].kind, ItemKind::Field);
        let flat = json!([{"name":"g","kind":12,"location":{"uri":"file:///a.rs","range":rg}}]);
        let s = document_symbols(&mut r, "file:///a.rs", flat).unwrap();
        assert_eq!(s[0].kind, ItemKind::Function);
        assert_eq!(s[0].range.end.line, 3);
    }

    #[test]
    fn diagnostics_convert_columns_and_defaults() {
        let uri = "file:///tmp/d.rs";
        let docs = docs_with(uri, "😀 x");
        let mut r = Resolver::new(&docs);
        let p = publish_diagnostics(
            &mut r,
            json!({"uri":uri,"version":4,"diagnostics":[
                {"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":4}},"severity":2,"code":"E1","source":"rustc","message":"m"},
                {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"code":404,"message":"n"}
            ]}),
        )
        .unwrap();
        assert_eq!(p.version, Some(4));
        assert_eq!(p.diagnostics[0].range.start, Position::new(0, 2));
        assert_eq!(p.diagnostics[0].severity, Severity::Warning);
        assert_eq!(p.diagnostics[0].code.as_deref(), Some("E1"));
        assert_eq!(p.diagnostics[1].severity, Severity::Error);
        assert_eq!(p.diagnostics[1].code.as_deref(), Some("404"));
    }

    #[test]
    fn drive_letter_spellings_find_the_same_doc() {
        let docs = docs_with("file:///C:/p/a.rs", "😀x");
        let mut r = Resolver::new(&docs);
        let p = r.pos(
            "file:///c%3A/p/a.rs",
            &WPos {
                line: 0,
                character: 2,
            },
        );
        assert_eq!(p, Position::new(0, 1));
    }

    #[test]
    fn malformed_is_protocol_error() {
        let docs = Docs::default();
        let mut r = Resolver::new(&docs);
        assert!(matches!(
            locations(&mut r, json!([{"uri":5}])),
            Err(LspError::Protocol(_))
        ));
    }
}
