//! `"remotes"` and `"ssh.autoforward"`: saved `skink ssh` targets and what
//! happens when a program on the remote starts listening on a port.

use serde_json::Value;

/// A port to forward: `remote` on the host, `local` here (loopback only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardSpec {
    pub remote: u16,
    pub local: u16,
}

impl ForwardSpec {
    /// `"3000"` (same port both ends) or `"5432:15432"` (remote:local).
    pub fn parse(s: &str) -> Option<Self> {
        let port = |p: &str| p.trim().parse::<u16>().ok().filter(|&n| n > 0);
        match s.split_once(':') {
            Some((r, l)) => Some(Self {
                remote: port(r)?,
                local: port(l)?,
            }),
            None => {
                let p = port(s)?;
                Some(Self {
                    remote: p,
                    local: p,
                })
            }
        }
    }
}

/// What to do when a port opens on the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoForward {
    /// Forward it right away and show the local URL.
    On,
    /// Show a toast offering `:forward PORT`.
    Ask,
    /// Only configured forwards and `:forward`.
    Off,
}

impl AutoForward {
    pub fn parse(v: &Value) -> Option<Self> {
        match v {
            Value::Bool(true) => Some(Self::On),
            Value::Bool(false) => Some(Self::Off),
            Value::String(s) if s == "ask" => Some(Self::Ask),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }
}

/// One saved remote: `skink ssh NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// `[user@]host[:port]`, as ssh takes it (a `~/.ssh/config` alias works).
    pub host: String,
    /// The folder to open there (empty: the remote home).
    pub path: String,
    pub forwards: Vec<ForwardSpec>,
    /// Overrides `"ssh.autoforward"` for this remote.
    pub autoforward: Option<AutoForward>,
}

const REMOTES_ERR: &str = "remotes must be {\"name\": {\"host\": \"user@host\", \"path\": \"~/app\", \"forwards\": [\"3000\", \"5432:15432\"], \"autoforward\": true|false|\"ask\"}}";

/// Parse `"remotes"`; errors name the remote and the field.
pub(crate) fn parse_remotes(v: &Value) -> Result<Vec<(String, Remote)>, String> {
    let obj = v.as_object().ok_or(REMOTES_ERR)?;
    let mut out = Vec::new();
    for (name, r) in obj {
        let r = r
            .as_object()
            .ok_or_else(|| format!("remotes.{name} must be an object with \"host\""))?;
        for k in r.keys() {
            if !["host", "path", "forwards", "autoforward"].contains(&k.as_str()) {
                return Err(format!("remotes.{name}: unknown field \"{k}\""));
            }
        }
        let host = r
            .get("host")
            .and_then(Value::as_str)
            .filter(|h| !h.trim().is_empty())
            .ok_or_else(|| {
                format!("remotes.{name}: \"host\" is required, e.g. \"user@10.0.0.5\"")
            })?;
        let path = match r.get("path") {
            None => String::new(),
            Some(Value::String(p)) => p.clone(),
            Some(_) => return Err(format!("remotes.{name}: \"path\" must be a string")),
        };
        let forwards = match r.get("forwards") {
            None => Vec::new(),
            Some(Value::Array(a)) => a
                .iter()
                .map(|f| {
                    let text = match f {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    };
                    ForwardSpec::parse(&text).ok_or_else(|| {
                        format!("remotes.{name}: forward {f} must be \"PORT\" or \"REMOTE:LOCAL\"")
                    })
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("remotes.{name}: \"forwards\" must be a list")),
        };
        let autoforward = match r.get("autoforward") {
            None => None,
            Some(v) => Some(AutoForward::parse(v).ok_or_else(|| {
                format!("remotes.{name}: \"autoforward\" must be true, false or \"ask\"")
            })?),
        };
        out.push((
            name.clone(),
            Remote {
                host: host.trim().to_owned(),
                path,
                forwards,
                autoforward,
            },
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remotes_parse_with_forwards_and_reasons() {
        let r = parse_remotes(&json!({"pi": {"host": "pi@10.0.0.5", "path": "~/app",
            "forwards": ["3000", "5432:15432", 8080], "autoforward": "ask"}}))
        .unwrap();
        assert_eq!(r[0].0, "pi");
        assert_eq!(
            r[0].1.forwards,
            [
                ForwardSpec {
                    remote: 3000,
                    local: 3000
                },
                ForwardSpec {
                    remote: 5432,
                    local: 15432
                },
                ForwardSpec {
                    remote: 8080,
                    local: 8080
                }
            ]
        );
        assert_eq!(r[0].1.autoforward, Some(AutoForward::Ask));
        for (bad, why) in [
            (json!({"pi": {}}), "host"),
            (json!({"pi": {"host": "h", "forwards": ["x"]}}), "forward"),
            (json!({"pi": {"host": "h", "port": 22}}), "unknown field"),
            (
                json!({"pi": {"host": "h", "autoforward": 1}}),
                "autoforward",
            ),
        ] {
            let e = parse_remotes(&bad).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
    }
}
