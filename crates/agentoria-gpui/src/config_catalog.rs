//! Bounded, read-only catalog of user/project hook and MCP definitions.
//! These are definitions on disk, not a simulation of an agent's runtime state.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::{json, Value};

const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const DIRECTORY_LIMIT: usize = 256;
const ENTRY_LIMIT: usize = 4096;
const DETAIL_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    Hooks,
    Mcp,
}

impl ConfigKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hooks => "Hooks",
            Self::Mcp => "MCP",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Self::Hooks => "hooks",
            Self::Mcp => "servers",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigAgent {
    Claude,
    Codex,
    Grok,
}

impl ConfigAgent {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Grok];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Grok => "Grok Build",
        }
    }
}

#[derive(Debug)]
pub struct ConfigEntry {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub origin: &'static str,
    pub scope: &'static str,
    pub path: PathBuf,
    pub location: String,
    pub detail: Arc<str>,
}

impl ConfigEntry {
    pub fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            [&self.name, &self.summary, self.origin, self.scope]
                .iter()
                .any(|field| field.to_lowercase().contains(&word))
                || self.path.to_string_lossy().to_lowercase().contains(&word)
        })
    }
}

#[derive(Default)]
pub struct ConfigScan {
    pub entries: Vec<ConfigEntry>,
    pub errors: Vec<String>,
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct ConfigRoots {
    pub home: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub grok: PathBuf,
}

impl ConfigRoots {
    pub fn new(home: PathBuf) -> Self {
        Self {
            claude: home.join(".claude"),
            codex: home.join(".codex"),
            grok: home.join(".grok"),
            home,
        }
    }

    pub fn from_environment(home: PathBuf) -> Self {
        let mut roots = Self::new(home);
        for (name, root) in [
            ("CLAUDE_CONFIG_DIR", &mut roots.claude),
            ("CODEX_HOME", &mut roots.codex),
            ("GROK_HOME", &mut roots.grok),
        ] {
            if let Some(path) = std::env::var_os(name).filter(|path| !path.is_empty()) {
                *root = PathBuf::from(path);
            }
        }
        roots
    }
}

pub fn discover(
    kind: ConfigKind,
    agent: ConfigAgent,
    roots: &ConfigRoots,
    project: Option<&Path>,
) -> ConfigScan {
    let mut scanner = Scanner {
        kind,
        scan: ConfigScan::default(),
        seen: HashSet::new(),
        detail_bytes: 0,
    };
    match agent {
        ConfigAgent::Claude => scanner.claude(roots, project, "Claude Code"),
        ConfigAgent::Codex => {
            for (directory, scope) in [(Some(roots.codex.as_path()), "User"), (project, "Project")]
            {
                let Some(directory) = directory else { continue };
                let directory = if scope == "Project" {
                    directory.join(".codex")
                } else {
                    directory.to_path_buf()
                };
                scanner.file(&directory.join("config.toml"), "Codex", scope);
                if kind == ConfigKind::Hooks {
                    scanner.file(&directory.join("hooks.json"), "Codex", scope);
                }
            }
        }
        ConfigAgent::Grok => {
            let config = scanner.file(&roots.grok.join("config.toml"), "Grok Build", "User");
            if kind == ConfigKind::Hooks {
                scanner.directory(&roots.grok.join("hooks"), "Grok Build", "User");
            }
            if let Some(project) = project {
                if kind == ConfigKind::Hooks {
                    scanner.directory(&project.join(".grok/hooks"), "Grok Build", "Project");
                } else {
                    scanner.file(&project.join(".grok/config.toml"), "Grok Build", "Project");
                }
            }
            let key = if kind == ConfigKind::Hooks {
                "hooks"
            } else {
                "mcps"
            };
            let compatible = |vendor: &str| {
                config
                    .as_ref()
                    .and_then(|value| value.get("compat"))
                    .and_then(|value| value.get(vendor))
                    .and_then(|value| value.get(key))
                    .and_then(Value::as_bool)
                    != Some(false)
            };
            if compatible("claude") {
                scanner.claude(roots, project, "Claude compatibility");
            } else if kind == ConfigKind::Mcp {
                // The root .mcp.json is independent of the Claude user-config switch.
                if let Some(project) = project {
                    scanner.file(&project.join(".mcp.json"), "MCP project", "Project");
                }
            }
            if compatible("cursor") {
                let filename = if kind == ConfigKind::Hooks {
                    "hooks.json"
                } else {
                    "mcp.json"
                };
                scanner.file(
                    &roots.home.join(".cursor").join(filename),
                    "Cursor compatibility",
                    "User",
                );
                if let Some(project) = project {
                    scanner.file(
                        &project.join(".cursor").join(filename),
                        "Cursor compatibility",
                        "Project",
                    );
                }
            }
        }
    }
    scanner.scan.entries.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.location.cmp(&right.location))
    });
    scanner.scan
}

struct Scanner {
    kind: ConfigKind,
    scan: ConfigScan,
    seen: HashSet<PathBuf>,
    detail_bytes: usize,
}

impl Scanner {
    fn claude(&mut self, roots: &ConfigRoots, project: Option<&Path>, origin: &'static str) {
        if self.kind == ConfigKind::Hooks {
            for filename in ["settings.json", "settings.local.json"] {
                self.file(&roots.claude.join(filename), origin, "User");
                if let Some(project) = project {
                    let scope = if filename == "settings.local.json" {
                        "Local"
                    } else {
                        "Project"
                    };
                    self.file(&project.join(".claude").join(filename), origin, scope);
                }
            }
        } else {
            // Claude stores both user and project-private MCP definitions in this file.
            let path = roots.home.join(".claude.json");
            if let Some(value) = self.file(&path, origin, "User") {
                if let Some(project) = project {
                    if let Some(value) = value
                        .get("projects")
                        .and_then(|projects| projects.get(project.to_string_lossy().as_ref()))
                    {
                        self.extract(
                            &path,
                            value,
                            origin,
                            "Local",
                            &format!("projects[{}].", project.display()),
                        );
                    }
                }
            }
            if let Some(project) = project {
                self.file(&project.join(".mcp.json"), origin, "Project");
            }
        }
    }

    fn directory(&mut self, path: &Path, origin: &'static str, scope: &'static str) {
        self.track_path(path);
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.scan
                    .errors
                    .push(format!("{}: {error}", path.display()));
                return;
            }
        };
        let mut paths = Vec::new();
        // Cap enumeration as well as reads; never recursively walk a user's home.
        for entry in entries.take(DIRECTORY_LIMIT + 1) {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(error) => self
                    .scan
                    .errors
                    .push(format!("{}: {error}", path.display())),
            }
        }
        if paths.len() > DIRECTORY_LIMIT {
            paths.truncate(DIRECTORY_LIMIT);
            self.scan.truncated = true;
        }
        paths.sort();
        for path in paths {
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path.is_file()
            {
                self.file(&path, origin, scope);
            }
        }
    }

    fn track_path(&mut self, path: &Path) {
        if !self.scan.paths.iter().any(|candidate| candidate == path) {
            self.scan.paths.push(path.to_path_buf());
        }
    }

    fn file(&mut self, path: &Path, origin: &'static str, scope: &'static str) -> Option<Value> {
        self.track_path(path);
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                self.scan.errors.push(format!(
                    "{}: not a regular configuration file",
                    path.display()
                ));
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                self.scan
                    .errors
                    .push(format!("{}: {error}", path.display()));
                return None;
            }
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                self.scan
                    .errors
                    .push(format!("{}: {error}", path.display()));
                return None;
            }
        };
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                self.scan
                    .errors
                    .push(format!("{}: {error}", path.display()));
                return None;
            }
        };
        if !self.seen.insert(canonical) {
            return None;
        }
        let mut text = String::new();
        if let Err(error) = file.take(FILE_LIMIT + 1).read_to_string(&mut text) {
            self.scan
                .errors
                .push(format!("{}: {error}", path.display()));
            return None;
        }
        if text.len() as u64 > FILE_LIMIT {
            self.scan.errors.push(format!(
                "{}: exceeds the 2 MiB configuration limit",
                path.display()
            ));
            return None;
        }
        let value = if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            // TOML Display errors include source snippets, which may contain credentials.
            toml::from_str::<toml::Value>(&text)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok())
        } else {
            serde_json::from_str::<Value>(&text).ok()
        };
        let Some(value) = value else {
            self.scan
                .errors
                .push(format!("{}: invalid configuration syntax", path.display()));
            return None;
        };
        self.extract(path, &value, origin, scope, "");
        Some(value)
    }

    fn extract(
        &mut self,
        path: &Path,
        value: &Value,
        origin: &'static str,
        scope: &'static str,
        prefix: &str,
    ) {
        match self.kind {
            ConfigKind::Hooks => {
                let Some(hooks) = value.get("hooks") else {
                    return;
                };
                let Some(hooks) = hooks.as_object() else {
                    self.scan
                        .errors
                        .push(format!("{}: hooks must be an object", path.display()));
                    return;
                };
                for (event, groups) in hooks {
                    // Codex stores hook state alongside inline definitions; state is not an event.
                    if event == "state" {
                        continue;
                    }
                    let Some(groups) = groups.as_array() else {
                        self.scan.errors.push(format!(
                            "{}: hooks.{event} must be an array",
                            path.display()
                        ));
                        continue;
                    };
                    for (group_index, group) in groups.iter().enumerate() {
                        if !group.is_object()
                            || group
                                .get("hooks")
                                .is_some_and(|handlers| !handlers.is_array())
                        {
                            self.scan.errors.push(format!(
                                "{}: invalid matcher group for {event}",
                                path.display()
                            ));
                            continue;
                        }
                        let handlers = group.get("hooks").and_then(Value::as_array);
                        // Do not clone the whole handler array for every selected handler.
                        let options = group
                            .as_object()
                            .map(|fields| {
                                fields
                                    .iter()
                                    .filter(|(key, _)| key.as_str() != "hooks")
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect::<serde_json::Map<_, _>>()
                            })
                            .unwrap_or_default();
                        for (index, handler) in handlers
                            .map(Vec::as_slice)
                            .unwrap_or_else(|| std::slice::from_ref(group))
                            .iter()
                            .enumerate()
                        {
                            if self.scan.entries.len() >= ENTRY_LIMIT
                                || self.detail_bytes >= DETAIL_BUDGET
                            {
                                self.scan.truncated = true;
                                return;
                            }
                            if !handler.is_object() {
                                self.scan.errors.push(format!(
                                    "{}: invalid handler for {event}",
                                    path.display()
                                ));
                                continue;
                            }
                            let matcher =
                                group.get("matcher").and_then(Value::as_str).unwrap_or("*");
                            let handler_type = handler
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("command");
                            let target = handler
                                .get("command")
                                .or_else(|| handler.get("url"))
                                .or_else(|| handler.get("prompt"))
                                .or_else(|| handler.get("tool"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let preview = redact_url(target)
                                .chars()
                                .take(256)
                                .collect::<String>()
                                .replace(['\n', '\r'], " ");
                            let definition = if handlers.is_some() {
                                let mut definition = Value::Object(options.clone());
                                definition["hooks"] = json!([handler]);
                                definition
                            } else {
                                handler.clone()
                            };
                            let location = if handlers.is_some() {
                                format!("{prefix}hooks.{event}[{group_index}].hooks[{index}]")
                            } else {
                                format!("{prefix}hooks.{event}[{group_index}]")
                            };
                            self.push(
                                path,
                                origin,
                                scope,
                                event.clone(),
                                format!("{handler_type} · {matcher} · {preview}"),
                                location,
                                json!({"hooks": {event: [definition]}}),
                            );
                        }
                    }
                }
            }
            ConfigKind::Mcp => {
                for key in ["mcpServers", "mcp_servers"] {
                    let Some(servers) = value.get(key) else {
                        continue;
                    };
                    let Some(servers) = servers.as_object() else {
                        self.scan
                            .errors
                            .push(format!("{}: {key} must be an object", path.display()));
                        continue;
                    };
                    for (name, server) in servers {
                        if !server.is_object() {
                            self.scan.errors.push(format!(
                                "{}: invalid MCP definition for {name}",
                                path.display()
                            ));
                            continue;
                        }
                        let transport = server
                            .get("type")
                            .or_else(|| server.get("transport"))
                            .and_then(Value::as_str)
                            .unwrap_or(if server.get("url").is_some() {
                                "HTTP"
                            } else {
                                "stdio"
                            });
                        let endpoint = server
                            .get("url")
                            .or_else(|| server.get("command"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.push(
                            path,
                            origin,
                            scope,
                            name.clone(),
                            format!("{transport} · {}", redact_url(endpoint)),
                            format!("{prefix}{key}.{name}"),
                            server.clone(),
                        );
                    }
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a definition's fields stay explicit at extraction sites"
    )]
    fn push(
        &mut self,
        path: &Path,
        origin: &'static str,
        scope: &'static str,
        name: String,
        summary: String,
        location: String,
        mut value: Value,
    ) {
        if self.scan.entries.len() >= ENTRY_LIMIT || self.detail_bytes >= DETAIL_BUDGET {
            self.scan.truncated = true;
            return;
        }
        redact(&mut value);
        let detail = match serde_json::to_string_pretty(&value) {
            Ok(detail) => detail,
            Err(_) => return,
        };
        if self.detail_bytes.saturating_add(detail.len()) > DETAIL_BUDGET {
            self.scan.truncated = true;
            return;
        }
        self.detail_bytes += detail.len();
        self.scan.entries.push(ConfigEntry {
            id: format!("{}:{location}", path.display()),
            name,
            summary,
            origin,
            scope,
            path: path.to_path_buf(),
            location,
            detail: detail.into(),
        });
    }
}

fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("apikey")
        || key == "authorization"
        || key == "credential"
}

fn redact_url(text: &str) -> String {
    let Ok(mut url) = url::Url::parse(text) else {
        return text.to_string();
    };
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return text.to_string();
    }
    if !url.username().is_empty() {
        let _ = url.set_username("REDACTED");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("REDACTED"));
    }
    if url.query().is_some() {
        url.set_query(Some("REDACTED"));
    }
    if url.fragment().is_some() {
        url.set_fragment(Some("REDACTED"));
    }
    url.to_string()
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if sensitive(key) {
                    *value = Value::String("[REDACTED]".to_string());
                } else if matches!(key.as_str(), "env" | "headers" | "http_headers") {
                    if let Some(fields) = value.as_object_mut() {
                        for value in fields.values_mut() {
                            *value = Value::String("[REDACTED]".to_string());
                        }
                    } else {
                        *value = Value::String("[REDACTED]".to_string());
                    }
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        Value::String(text) => *text = redact_url(text),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn hooks_keep_matchers_and_distinct_handlers_without_writing_sources() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        let path = roots.claude.join("settings.json");
        let text = r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"first"},{"type":"http","url":"https://example.com/hook"}]}]}}"#;
        write(&path, text);
        let scan = discover(ConfigKind::Hooks, ConfigAgent::Claude, &roots, None);
        assert!(scan.errors.is_empty());
        assert_eq!(scan.entries.len(), 2);
        assert_ne!(scan.entries[0].id, scan.entries[1].id);
        assert!(scan.entries[1].detail.contains("Edit|Write"));
        assert!(scan.entries[1].detail.contains("PostToolUse"));
        assert!(scan.entries[0].matches("posttool claude edit"));
        assert!(scan.entries[0].matches("first"));
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn claude_mcp_lists_all_three_scopes_and_preserves_duplicate_names() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        let project = home.path().join("project");
        write(&roots.home.join(".claude.json"), &json!({
            "mcpServers":{"docs":{"type":"http","url":"https://user.example/mcp"}},
            "projects":{project.to_string_lossy().as_ref():{"mcpServers":{"docs":{"command":"private-server"}}}}
        }).to_string());
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"shared-server"}}}"#,
        );
        let scan = discover(ConfigKind::Mcp, ConfigAgent::Claude, &roots, Some(&project));
        assert!(scan.errors.is_empty());
        assert_eq!(scan.entries.len(), 3);
        assert_eq!(
            scan.entries
                .iter()
                .map(|entry| entry.scope)
                .collect::<HashSet<_>>(),
            HashSet::from(["User", "Local", "Project"])
        );
        assert_eq!(
            discover(ConfigKind::Mcp, ConfigAgent::Claude, &roots, None)
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn codex_reads_toml_and_json_but_does_not_treat_trust_state_as_hooks() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        write(&roots.codex.join("config.toml"), "[mcp_servers.docs]\nurl='https://example.com/mcp'\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype='command'\ncommand='stop'\n[hooks.state.sample]\nenabled=true\n");
        write(
            &roots.codex.join("hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"start"}]}]}}"#,
        );
        let hooks = discover(ConfigKind::Hooks, ConfigAgent::Codex, &roots, None);
        assert!(hooks.errors.is_empty());
        assert_eq!(hooks.entries.len(), 2);
        assert_eq!(
            discover(ConfigKind::Mcp, ConfigAgent::Codex, &roots, None).entries[0].name,
            "docs"
        );
    }

    #[test]
    fn grok_reads_native_and_compatibility_sources_and_honors_compat_switch() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        write(
            &roots.grok.join("hooks/native.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"native"}]}]}}"#,
        );
        write(
            &roots.claude.join("settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"compat"}]}]}}"#,
        );
        write(
            &roots.home.join(".cursor/hooks.json"),
            r#"{"hooks":{"beforeShellExecution":[{"command":"cursor"}]}}"#,
        );
        let hooks = discover(ConfigKind::Hooks, ConfigAgent::Grok, &roots, None);
        assert!(hooks.errors.is_empty());
        assert_eq!(hooks.entries.len(), 3);
        assert!(hooks
            .entries
            .iter()
            .any(|entry| entry.location == "hooks.beforeShellExecution[0]"));
        write(
            &roots.grok.join("config.toml"),
            "[compat.claude]\nhooks=false\n[compat.cursor]\nhooks=false\n",
        );
        assert_eq!(
            discover(ConfigKind::Hooks, ConfigAgent::Grok, &roots, None)
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn credentials_are_removed_before_details_are_retained() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        write(&roots.codex.join("config.toml"), "[mcp_servers.remote]\nurl='https://alice:password@example.com/mcp?key=query-secret#fragment-secret'\nbearer_token='token-secret'\n[mcp_servers.remote.env]\nCUSTOM='env-secret'\n[mcp_servers.remote.http_headers]\nCustom='header-secret'\n");
        let scan = discover(ConfigKind::Mcp, ConfigAgent::Codex, &roots, None);
        assert_eq!(scan.entries.len(), 1);
        let entry = &scan.entries[0];
        for secret in [
            "alice",
            "password",
            "query-secret",
            "fragment-secret",
            "token-secret",
            "env-secret",
            "header-secret",
        ] {
            assert!(!entry.detail.contains(secret), "detail leaked {secret}");
            assert!(!entry.summary.contains(secret), "summary leaked {secret}");
        }
    }

    #[test]
    fn malformed_and_oversized_files_are_reported_without_source_snippets() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        write(
            &roots.codex.join("config.toml"),
            "credential = 'do-not-display'\nbroken = [",
        );
        write(
            &roots.codex.join("hooks.json"),
            &" ".repeat(FILE_LIMIT as usize + 1),
        );
        let scan = discover(ConfigKind::Hooks, ConfigAgent::Codex, &roots, None);
        assert_eq!(scan.errors.len(), 2);
        assert!(scan.entries.is_empty());
        assert!(!scan.errors.join("\n").contains("do-not-display"));
    }

    #[test]
    fn definition_and_detail_limits_are_reported() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        let handler = json!({"type":"command", "command":"true"});
        write(
            &roots.codex.join("hooks.json"),
            &json!({"hooks":{"Stop":[{"hooks":vec![handler; ENTRY_LIMIT + 1]}]}}).to_string(),
        );
        let scan = discover(ConfigKind::Hooks, ConfigAgent::Codex, &roots, None);
        assert_eq!(scan.entries.len(), ENTRY_LIMIT);
        assert!(scan.truncated);

        let definition =
            json!({"hooks":{"Stop":[{"command":"true", "description":"x".repeat(512 * 1024)}]}})
                .to_string();
        for index in 0..18 {
            write(
                &roots.grok.join(format!("hooks/{index:02}.json")),
                &definition,
            );
        }
        let scan = discover(ConfigKind::Hooks, ConfigAgent::Grok, &roots, None);
        assert!(scan.truncated);
        assert!(!scan.entries.is_empty());
        assert!(scan.entries.len() < 18);
        assert!(
            scan.entries
                .iter()
                .map(|entry| entry.detail.len())
                .sum::<usize>()
                <= DETAIL_BUDGET
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliased_hook_files_are_read_once() {
        let home = tempdir().unwrap();
        let roots = ConfigRoots::new(home.path().into());
        let path = roots.grok.join("hooks/native.json");
        write(&path, r#"{"hooks":{"Stop":[{"command":"true"}]}}"#);
        std::os::unix::fs::symlink(&path, roots.grok.join("hooks/alias.json")).unwrap();
        assert_eq!(
            discover(ConfigKind::Hooks, ConfigAgent::Grok, &roots, None)
                .entries
                .len(),
            1
        );
    }
}
