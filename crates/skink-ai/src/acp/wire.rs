//! Hand-written subset of the Agent Client Protocol wire types (protocol v1).
//!
//! Field names and enum spellings are copied from the official schema crate
//! `agent-client-protocol-schema` v1.15.1 (Apache-2.0, Copyright (c) Zed
//! Industries), `src/v1/{agent,client,content,tool_call,plan}.rs`, and the spec
//! at <https://agentclientprotocol.com>. Changes: only the subset Skink needs,
//! deserialisation is maximally lenient (every optional field defaults, unknown
//! enum values map to `Other`/`Unknown`) and there is no `schemars` /
//! `serde_with` / `derive_more` dependency tree. See `NOTICES.md`.
//!
//! Requests are built with `serde_json::json!` in `client.rs`; these types are
//! for what comes *from* the agent.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::agent::{
    AgentInfo, AuthMethodInfo, CommandInfo, ModeInfo, ModelInfo, PermissionKind, PermissionOption,
    PlanEntry, PlanStatus, SessionRemoval, StopReason, ToolCallInfo, ToolCallPatch, ToolContent,
    ToolKind, ToolLocation, ToolStatus,
};

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u32 = 1;

// Method names (src/v1/agent.rs, client.rs).
pub const M_INITIALIZE: &str = "initialize";
pub const M_AUTHENTICATE: &str = "authenticate";
pub const M_SESSION_NEW: &str = "session/new";
pub const M_SESSION_LOAD: &str = "session/load";
pub const M_SESSION_PROMPT: &str = "session/prompt";
pub const M_SESSION_CANCEL: &str = "session/cancel";
pub const M_SESSION_SET_MODE: &str = "session/set_mode";
/// Unstable in ACP v1 (agents that list `models` in `session/new`).
pub const M_SESSION_SET_MODEL: &str = "session/set_model";
pub const M_SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
pub const M_SESSION_CLOSE: &str = "session/close";
pub const M_SESSION_DELETE: &str = "session/delete";
pub const M_SESSION_UPDATE: &str = "session/update";
pub const M_REQUEST_PERMISSION: &str = "session/request_permission";
pub const M_FS_READ: &str = "fs/read_text_file";
pub const M_FS_WRITE: &str = "fs/write_text_file";
pub const M_TERMINAL_CREATE: &str = "terminal/create";
pub const M_TERMINAL_OUTPUT: &str = "terminal/output";
pub const M_TERMINAL_RELEASE: &str = "terminal/release";
pub const M_TERMINAL_WAIT: &str = "terminal/wait_for_exit";
pub const M_TERMINAL_KILL: &str = "terminal/kill";

// JSON-RPC / ACP error codes.
pub const E_PARSE: i64 = -32700;
pub const E_METHOD_NOT_FOUND: i64 = -32601;
pub const E_INVALID_PARAMS: i64 = -32602;
pub const E_INTERNAL: i64 = -32603;
pub const E_AUTH_REQUIRED: i64 = -32000;
pub const E_RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Implementation {
    name: String,
    title: Option<String>,
    version: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PromptCaps {
    image: bool,
    embedded_context: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct AgentCaps {
    load_session: bool,
    prompt_capabilities: PromptCaps,
    session_capabilities: SessionCaps,
}

/// `agentCapabilities.sessionCapabilities`: `{}` means supported, absent or
/// `null` means not.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct SessionCaps {
    close: Option<Value>,
    delete: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct WireAuthMethod {
    id: String,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct InitializeResult {
    protocol_version: Value,
    agent_capabilities: AgentCaps,
    auth_methods: Vec<WireAuthMethod>,
    agent_info: Option<Implementation>,
}

/// Parse an `initialize` result. `Err` carries the version the agent chose
/// when it differs from ours.
pub fn parse_initialize(v: Value) -> Result<AgentInfo, String> {
    let r: InitializeResult =
        serde_json::from_value(v).map_err(|e| format!("bad initialize result: {e}"))?;
    let ver = r.protocol_version.as_u64().unwrap_or(0);
    if ver != u64::from(PROTOCOL_VERSION) {
        return Err(format!(
            "agent speaks ACP protocol version {ver}, this client speaks {PROTOCOL_VERSION}"
        ));
    }
    let imp = r.agent_info.unwrap_or_default();
    Ok(AgentInfo {
        name: imp.name,
        title: imp.title,
        version: imp.version,
        protocol_version: PROTOCOL_VERSION,
        load_session: r.agent_capabilities.load_session,
        image_prompts: r.agent_capabilities.prompt_capabilities.image,
        embedded_context: r.agent_capabilities.prompt_capabilities.embedded_context,
        auth_methods: r
            .auth_methods
            .into_iter()
            .map(|a| AuthMethodInfo {
                id: a.id,
                name: a.name,
                description: a.description,
            })
            .collect(),
        session_removal: {
            let caps = &r.agent_capabilities.session_capabilities;
            if caps.close.is_some() {
                SessionRemoval::Close
            } else if caps.delete.is_some() {
                SessionRemoval::Delete
            } else {
                SessionRemoval::Unsupported
            }
        },
    })
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireMode {
    id: String,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireModes {
    current_mode_id: Option<String>,
    available_modes: Vec<WireMode>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireModel {
    model_id: String,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireModels {
    current_model_id: Option<String>,
    available_models: Vec<WireModel>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct NewSessionResult {
    session_id: Option<String>,
    modes: Option<WireModes>,
    models: Option<WireModels>,
    config_options: Option<Vec<Value>>,
}

/// A session configuration option (ACP `configOptions`), select kind only:
/// the values flattened out of any groups, as (value, name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub values: Vec<(String, String)>,
}

impl ConfigOption {
    /// A reasoning / thinking level selector: category `thought_level`, or
    /// an uncategorized option whose id or name says effort or thinking.
    pub fn is_thought_level(&self) -> bool {
        match self.category.as_deref() {
            Some("thought_level") => true,
            Some(_) => false,
            None => {
                let s = crate::agent::normalize(&format!("{} {}", self.id, self.name));
                ["effort", "thinking", "reasoning", "thought"]
                    .iter()
                    .any(|w| s.contains(w))
            }
        }
    }
}

fn parse_config_option(v: &Value) -> Option<ConfigOption> {
    if v.get("type").and_then(Value::as_str) != Some("select") {
        return None;
    }
    let pair = |o: &Value| -> Option<(String, String)> {
        let value = o.get("value")?.as_str()?.to_owned();
        let name = o
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&value)
            .to_owned();
        Some((value, name))
    };
    let mut values = Vec::new();
    for o in v.get("options")?.as_array()? {
        match o.get("options").and_then(Value::as_array) {
            Some(group) => values.extend(group.iter().filter_map(pair)),
            None => values.extend(pair(o)),
        }
    }
    Some(ConfigOption {
        id: v.get("id")?.as_str()?.to_owned(),
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        category: v.get("category").and_then(Value::as_str).map(str::to_owned),
        values,
    })
}

/// A parsed `session/new` / `session/load` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSession {
    pub id: String,
    pub modes: Vec<ModeInfo>,
    pub current_mode: Option<String>,
    pub models: Vec<ModelInfo>,
    pub current_model: Option<String>,
    pub config_options: Vec<ConfigOption>,
}

/// `session/new` (and `session/load`) result.
pub fn parse_session(v: Value, known_id: Option<&str>) -> Result<ParsedSession, String> {
    // session/load returns `null` or an object without sessionId.
    let r: NewSessionResult = if v.is_null() {
        NewSessionResult::default()
    } else {
        serde_json::from_value(v).map_err(|e| format!("bad session result: {e}"))?
    };
    let id = r
        .session_id
        .or_else(|| known_id.map(str::to_owned))
        .ok_or("session/new result has no sessionId")?;
    let (modes, cur) = match r.modes {
        Some(m) => (
            m.available_modes
                .into_iter()
                .map(|m| ModeInfo {
                    id: m.id,
                    name: m.name,
                    description: m.description,
                })
                .collect(),
            m.current_mode_id,
        ),
        None => (Vec::new(), None),
    };
    let (models, current_model) = match r.models {
        Some(m) => (
            m.available_models
                .into_iter()
                .map(|m| ModelInfo {
                    id: m.model_id,
                    name: m.name,
                    description: m.description,
                })
                .collect(),
            m.current_model_id,
        ),
        None => (Vec::new(), None),
    };
    let config_options = r
        .config_options
        .unwrap_or_default()
        .iter()
        .filter_map(parse_config_option)
        .collect();
    Ok(ParsedSession {
        id,
        modes,
        current_mode: cur,
        models,
        current_model,
        config_options,
    })
}

pub fn parse_stop_reason(v: &Value) -> StopReason {
    match v.get("stopReason").and_then(Value::as_str) {
        Some("end_turn") => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("max_turn_requests") => StopReason::MaxTurnRequests,
        Some("refusal") => StopReason::Refusal,
        Some("cancelled") => StopReason::Cancelled,
        Some(o) => StopReason::Other(o.to_owned()),
        None => StopReason::Other("missing".to_owned()),
    }
}

// ---- content ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    Image {},
    Audio {},
    ResourceLink {
        #[serde(default)]
        uri: String,
        #[serde(default)]
        name: String,
    },
    Resource {
        #[serde(default)]
        resource: Value,
    },
    #[serde(other)]
    Unknown,
}

/// Flatten a content block to display text; non-text become short placeholders.
fn block_text(v: Value) -> String {
    match serde_json::from_value::<Block>(v) {
        Ok(Block::Text { text }) => text,
        Ok(Block::Image {}) => "[image]".into(),
        Ok(Block::Audio {}) => "[audio]".into(),
        Ok(Block::ResourceLink { uri, name }) => {
            if name.is_empty() {
                uri
            } else {
                name
            }
        }
        Ok(Block::Resource { resource }) => resource
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default(),
        Ok(Block::Unknown) | Err(_) => String::new(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireToolContent {
    Content {
        #[serde(default)]
        content: Value,
    },
    #[serde(rename_all = "camelCase")]
    Diff {
        path: PathBuf,
        #[serde(default)]
        old_text: Option<String>,
        #[serde(default)]
        new_text: String,
    },
    #[serde(rename_all = "camelCase")]
    Terminal {
        #[serde(default)]
        terminal_id: String,
    },
    #[serde(other)]
    Unknown,
}

fn tool_content(items: Vec<Value>) -> Vec<ToolContent> {
    items
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<WireToolContent>(v) {
            Ok(WireToolContent::Content { content }) => {
                Some(ToolContent::Text(block_text(content)))
            }
            Ok(WireToolContent::Diff {
                path,
                old_text,
                new_text,
            }) => Some(ToolContent::Diff {
                path,
                old: old_text,
                new: new_text,
            }),
            Ok(WireToolContent::Terminal { terminal_id }) => {
                Some(ToolContent::Terminal(terminal_id))
            }
            Ok(WireToolContent::Unknown) | Err(_) => None,
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct WireLocation {
    path: PathBuf,
    #[serde(default)]
    line: Option<u32>,
}

fn locations(items: Vec<WireLocation>) -> Vec<ToolLocation> {
    items
        .into_iter()
        .map(|l| ToolLocation {
            path: l.path,
            line: l.line,
        })
        .collect()
}

fn kind_of(s: &str) -> ToolKind {
    match s {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        "switch_mode" => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

fn status_of(s: &str) -> ToolStatus {
    match s {
        "in_progress" => ToolStatus::InProgress,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

/// Shared by `tool_call`, `tool_call_update` and the permission request's
/// `toolCall` (all fields optional except the id).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireTool {
    tool_call_id: String,
    title: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    content: Option<Vec<Value>>,
    locations: Option<Vec<WireLocation>>,
}

impl WireTool {
    fn into_patch(self) -> ToolCallPatch {
        ToolCallPatch {
            id: self.tool_call_id,
            title: self.title,
            kind: self.kind.as_deref().map(kind_of),
            status: self.status.as_deref().map(status_of),
            content: self.content.map(tool_content),
            locations: self.locations.map(locations),
        }
    }
    fn into_info(self) -> ToolCallInfo {
        let p = self.into_patch();
        ToolCallInfo {
            id: p.id,
            title: p.title.unwrap_or_default(),
            kind: p.kind.unwrap_or_default(),
            status: p.status.unwrap_or_default(),
            content: p.content.unwrap_or_default(),
            locations: p.locations.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireChunk {
    #[serde(default)]
    content: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePlanEntry {
    #[serde(default)]
    content: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct WireCommand {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
enum WireUpdate {
    UserMessageChunk(WireChunk),
    AgentMessageChunk(WireChunk),
    AgentThoughtChunk(WireChunk),
    ToolCall(WireTool),
    ToolCallUpdate(WireTool),
    Plan {
        #[serde(default)]
        entries: Vec<WirePlanEntry>,
    },
    #[serde(rename_all = "camelCase")]
    AvailableCommandsUpdate {
        #[serde(default)]
        available_commands: Vec<WireCommand>,
    },
    #[serde(rename_all = "camelCase")]
    CurrentModeUpdate {
        #[serde(default)]
        current_mode_id: String,
    },
    /// usage_update, session_info_update, config_option_update, future kinds.
    #[serde(other)]
    Unknown,
}

/// A decoded `session/update`, in the editor-facing vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    User(String),
    Message(String),
    Thought(String),
    ToolCall(ToolCallInfo),
    ToolCallUpdate(ToolCallPatch),
    Plan(Vec<PlanEntry>),
    Commands(Vec<CommandInfo>),
    Mode(String),
    /// A kind we do not model; ignored.
    Ignored,
}

/// Decode `params` of a `session/update` notification into (sessionId, update).
pub fn parse_session_update(params: Value) -> Result<(String, Update), String> {
    let session = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("session/update without sessionId")?
        .to_owned();
    let upd = params
        .get("update")
        .cloned()
        .ok_or("session/update without update")?;
    let w: WireUpdate =
        serde_json::from_value(upd).map_err(|e| format!("bad session/update: {e}"))?;
    let u = match w {
        WireUpdate::UserMessageChunk(c) => Update::User(block_text(c.content)),
        WireUpdate::AgentMessageChunk(c) => Update::Message(block_text(c.content)),
        WireUpdate::AgentThoughtChunk(c) => Update::Thought(block_text(c.content)),
        WireUpdate::ToolCall(t) => Update::ToolCall(t.into_info()),
        WireUpdate::ToolCallUpdate(t) => Update::ToolCallUpdate(t.into_patch()),
        WireUpdate::Plan { entries } => Update::Plan(
            entries
                .into_iter()
                .map(|e| PlanEntry {
                    content: e.content,
                    priority: if e.priority.is_empty() {
                        "medium".into()
                    } else {
                        e.priority
                    },
                    status: match e.status.as_str() {
                        "in_progress" => PlanStatus::InProgress,
                        "completed" => PlanStatus::Completed,
                        _ => PlanStatus::Pending,
                    },
                })
                .collect(),
        ),
        WireUpdate::AvailableCommandsUpdate { available_commands } => Update::Commands(
            available_commands
                .into_iter()
                .map(|c| CommandInfo {
                    name: c.name,
                    description: c.description,
                    input_hint: c
                        .input
                        .as_ref()
                        .and_then(|i| i.get("hint"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect(),
        ),
        WireUpdate::CurrentModeUpdate { current_mode_id } => Update::Mode(current_mode_id),
        WireUpdate::Unknown => Update::Ignored,
    };
    Ok((session, u))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireOption {
    option_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePermission {
    session_id: String,
    #[serde(default)]
    tool_call: WireTool,
    #[serde(default)]
    options: Vec<WireOption>,
}

/// Decode `session/request_permission` params.
pub fn parse_permission(
    params: Value,
) -> Result<(String, ToolCallPatch, Vec<PermissionOption>), String> {
    let p: WirePermission = serde_json::from_value(params)
        .map_err(|e| format!("bad session/request_permission: {e}"))?;
    let options = p
        .options
        .into_iter()
        .map(|o| PermissionOption {
            id: o.option_id,
            name: o.name,
            kind: match o.kind.as_str() {
                "allow_once" => PermissionKind::AllowOnce,
                "allow_always" => PermissionKind::AllowAlways,
                "reject_once" => PermissionKind::RejectOnce,
                "reject_always" => PermissionKind::RejectAlways,
                _ => PermissionKind::Other,
            },
        })
        .collect();
    Ok((p.session_id, p.tool_call.into_patch(), options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_ok_and_version_mismatch() {
        let ok = parse_initialize(json!({
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": true, "embeddedContext": true}, "futureField": 1},
            "authMethods": [{"id": "oauth", "name": "Log in", "description": "browser"}],
            "agentInfo": {"name": "fake", "version": "0.1"}
        }))
        .unwrap();
        assert!(ok.load_session && ok.image_prompts && ok.embedded_context);
        assert_eq!(ok.auth_methods[0].id, "oauth");
        assert_eq!(ok.name, "fake");
        assert!(parse_initialize(json!({"protocolVersion": 2})).is_err());
        assert!(parse_initialize(json!({})).is_err());
    }

    #[test]
    fn updates_decode_and_unknown_is_ignored() {
        let (s, u) = parse_session_update(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}
        }))
        .unwrap();
        assert_eq!((s.as_str(), u), ("s1", Update::Message("hi".into())));

        let (_, u) = parse_session_update(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "usage_update", "used": 3}
        }))
        .unwrap();
        assert_eq!(u, Update::Ignored);

        let (_, u) = parse_session_update(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit a.rs",
                       "kind": "edit", "status": "in_progress",
                       "content": [{"type": "diff", "path": "/w/a.rs", "oldText": "a", "newText": "b"},
                                   {"type": "weird"}],
                       "locations": [{"path": "/w/a.rs", "line": 3}]}
        }))
        .unwrap();
        match u {
            Update::ToolCall(c) => {
                assert_eq!(c.kind, ToolKind::Edit);
                assert_eq!(c.status, ToolStatus::InProgress);
                assert_eq!(c.content.len(), 1);
                assert_eq!(c.locations[0].line, Some(3));
            }
            o => panic!("{o:?}"),
        }
        assert!(parse_session_update(json!({"update": {}})).is_err());
    }

    #[test]
    fn tool_update_is_partial() {
        let (_, u) = parse_session_update(json!({
            "sessionId": "s",
            "update": {"sessionUpdate": "tool_call_update", "toolCallId": "t", "status": "completed"}
        }))
        .unwrap();
        let Update::ToolCallUpdate(p) = u else {
            panic!()
        };
        assert_eq!(p.status, Some(ToolStatus::Completed));
        assert_eq!(p.title, None);
        assert_eq!(p.content, None);
    }

    #[test]
    fn plan_commands_mode() {
        let (_, u) = parse_session_update(json!({"sessionId":"s","update":{"sessionUpdate":"plan",
            "entries":[{"content":"a","priority":"high","status":"in_progress"},{"content":"b","priority":"low","status":"pending"}]}})).unwrap();
        let Update::Plan(e) = u else { panic!() };
        assert_eq!(e[0].status, PlanStatus::InProgress);
        let (_, u) = parse_session_update(
            json!({"sessionId":"s","update":{"sessionUpdate":"available_commands_update",
            "availableCommands":[{"name":"web","description":"search","input":{"hint":"query"}}]}}),
        )
        .unwrap();
        let Update::Commands(c) = u else { panic!() };
        assert_eq!(c[0].input_hint.as_deref(), Some("query"));
        let (_, u) = parse_session_update(json!({"sessionId":"s","update":{"sessionUpdate":"current_mode_update","currentModeId":"plan"}})).unwrap();
        assert_eq!(u, Update::Mode("plan".into()));
    }

    #[test]
    fn permission_decodes() {
        let (s, t, o) = parse_permission(json!({
            "sessionId": "s", "toolCall": {"toolCallId": "t9", "title": "rm -rf"},
            "options": [{"optionId": "a", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "r", "name": "No", "kind": "reject_always"},
                        {"optionId": "x", "name": "?", "kind": "new_kind"}]
        }))
        .unwrap();
        assert_eq!((s.as_str(), t.id.as_str()), ("s", "t9"));
        assert_eq!(o[1].kind, PermissionKind::RejectAlways);
        assert_eq!(o[2].kind, PermissionKind::Other);
    }

    #[test]
    fn session_result_variants() {
        let p = parse_session(
            json!({"sessionId": "abc", "modes": {"currentModeId": "m", "availableModes": [{"id": "m", "name": "M"}]},
                   "models": {"currentModelId": "fast", "availableModels": [{"modelId": "fast", "name": "Fast"}, {"modelId": "big", "name": "Big"}]}}),
            None,
        )
        .unwrap();
        assert_eq!(
            (p.id.as_str(), p.modes.len(), p.current_mode.as_deref()),
            ("abc", 1, Some("m"))
        );
        assert_eq!(p.models.len(), 2);
        assert_eq!(p.current_model.as_deref(), Some("fast"));
        // session/load: null result keeps the id we asked for
        let p = parse_session(Value::Null, Some("old")).unwrap();
        assert_eq!(p.id, "old");
        assert!(parse_session(json!({}), None).is_err());
    }

    #[test]
    fn stop_reasons() {
        assert_eq!(
            parse_stop_reason(&json!({"stopReason": "cancelled"})),
            StopReason::Cancelled
        );
        assert_eq!(
            parse_stop_reason(&json!({"stopReason": "zzz"})),
            StopReason::Other("zzz".into())
        );
    }
}
