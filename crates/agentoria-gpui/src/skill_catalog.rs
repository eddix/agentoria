use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _, Result};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

const METADATA_LIMIT: u64 = 64 * 1024;
const DETAIL_LIMIT: u64 = 2 * 1024 * 1024;
const IMAGE_LIMIT: u64 = 20 * 1024 * 1024;
const TREE_ENTRY_LIMIT: usize = 4096;
const TREE_DEPTH_LIMIT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAgent {
    Claude,
    Codex,
    Grok,
    Kimi,
    ZCode,
    Trae,
}

impl SkillAgent {
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Grok,
        Self::Kimi,
        Self::ZCode,
        Self::Trae,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Grok => "Grok Build",
            Self::Kimi => "Kimi",
            Self::ZCode => "ZCode",
            Self::Trae => "Trae",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    User,
    Shared,
    Project,
    BuiltIn,
}

impl SkillScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Shared => "Shared",
            Self::Project => "Project",
            Self::BuiltIn => "Built-in",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub origin: &'static str,
    pub scope: SkillScope,
    pub flat_markdown: bool,
}

#[derive(Debug, Clone)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub origin: &'static str,
    pub scope: SkillScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillFileKind {
    Markdown,
    Code(Option<&'static str>),
    Image,
}

#[derive(Debug, Clone)]
pub struct SkillFileEntry {
    pub path: PathBuf,
    pub relative: PathBuf,
    pub depth: usize,
    pub kind: Option<SkillFileKind>,
}

#[derive(Debug, Clone)]
pub struct SkillBundle {
    pub root: PathBuf,
    pub entry_file: PathBuf,
    pub files: Vec<SkillFileEntry>,
    pub truncated: bool,
}

impl SkillBundle {
    pub fn file(&self, path: &Path) -> Option<&SkillFileEntry> {
        self.files
            .iter()
            .find(|entry| entry.kind.is_some() && entry.path == path)
    }

    pub fn visible_indices(&self, collapsed: &HashSet<PathBuf>) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !collapsed
                    .iter()
                    .any(|folder| entry.relative != *folder && entry.relative.starts_with(folder))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

pub enum SkillLink {
    Local {
        path: PathBuf,
        anchor: Option<String>,
    },
    External(String),
    Unsupported,
}

#[derive(Debug, Default)]
pub struct SkillScan {
    pub skills: Vec<SkillSummary>,
    pub errors: Vec<String>,
}

#[derive(Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

pub fn roots(agent: SkillAgent, home: &Path, project: Option<&Path>) -> Vec<SkillRoot> {
    let mut roots = Vec::new();
    let mut add = |path: PathBuf, origin, scope, flat_markdown| {
        roots.push(SkillRoot {
            path,
            origin,
            scope,
            flat_markdown,
        });
    };

    match agent {
        SkillAgent::Claude => add(
            home.join(".claude/skills"),
            "Claude Code",
            SkillScope::User,
            false,
        ),
        SkillAgent::Codex => {
            add(home.join(".codex/skills"), "Codex", SkillScope::User, false);
            add(
                home.join(".agents/skills"),
                "Agent Skills",
                SkillScope::Shared,
                false,
            );
            add(
                home.join(".config/agents/skills"),
                "Agent Skills",
                SkillScope::Shared,
                false,
            );
        }
        SkillAgent::Grok => {
            add(
                home.join(".grok/bundled/skills"),
                "Grok built-in",
                SkillScope::BuiltIn,
                false,
            );
            for (directory, origin, scope) in [
                (".grok", "Grok Build", SkillScope::User),
                (".agents", "Agent Skills", SkillScope::Shared),
                (".claude", "Claude Code", SkillScope::Shared),
                (".cursor", "Cursor", SkillScope::Shared),
            ] {
                add(home.join(directory).join("skills"), origin, scope, false);
                add(home.join(directory).join("commands"), origin, scope, true);
            }
        }
        SkillAgent::Kimi => {
            add(home.join(".kimi/skills"), "Kimi", SkillScope::User, true);
            add(
                home.join(".claude/skills"),
                "Claude Code",
                SkillScope::Shared,
                true,
            );
            add(
                home.join(".codex/skills"),
                "Codex",
                SkillScope::Shared,
                true,
            );
            let generic = home.join(".config/agents/skills");
            let generic = if generic.is_dir() {
                generic
            } else {
                home.join(".agents/skills")
            };
            add(generic, "Agent Skills", SkillScope::Shared, true);
        }
        SkillAgent::ZCode => add(home.join(".zcode/skills"), "ZCode", SkillScope::User, false),
        SkillAgent::Trae => {
            add(
                home.join(".trae-cn/skills"),
                "Trae",
                SkillScope::User,
                false,
            );
            add(home.join(".trae/skills"), "Trae", SkillScope::User, false);
            add(
                home.join(".traecli/skills"),
                "Trae CLI",
                SkillScope::User,
                false,
            );
            add(
                home.join(".trae/builtin/global/skills"),
                "Trae built-in",
                SkillScope::BuiltIn,
                false,
            );
        }
    }

    if let Some(project) = project {
        match agent {
            SkillAgent::Claude => add(
                project.join(".claude/skills"),
                "Claude Code",
                SkillScope::Project,
                false,
            ),
            SkillAgent::Codex => {
                add(
                    project.join(".codex/skills"),
                    "Codex",
                    SkillScope::Project,
                    false,
                );
                add(
                    project.join(".agents/skills"),
                    "Agent Skills",
                    SkillScope::Project,
                    false,
                );
            }
            SkillAgent::Grok => {
                for (directory, origin) in [
                    (".grok", "Grok Build"),
                    (".agents", "Agent Skills"),
                    (".claude", "Claude Code"),
                    (".cursor", "Cursor"),
                ] {
                    add(
                        project.join(directory).join("skills"),
                        origin,
                        SkillScope::Project,
                        false,
                    );
                    add(
                        project.join(directory).join("commands"),
                        origin,
                        SkillScope::Project,
                        true,
                    );
                }
            }
            SkillAgent::Kimi => {
                for (directory, origin) in [
                    (".kimi/skills", "Kimi"),
                    (".claude/skills", "Claude Code"),
                    (".codex/skills", "Codex"),
                    (".agents/skills", "Agent Skills"),
                ] {
                    add(project.join(directory), origin, SkillScope::Project, true);
                }
            }
            SkillAgent::ZCode => add(
                project.join(".zcode/skills"),
                "ZCode",
                SkillScope::Project,
                false,
            ),
            SkillAgent::Trae => {
                add(
                    project.join(".trae/skills"),
                    "Trae",
                    SkillScope::Project,
                    false,
                );
                add(
                    project.join(".traecli/skills"),
                    "Trae CLI",
                    SkillScope::Project,
                    false,
                );
            }
        }
    }

    roots
}

pub fn discover(roots: &[SkillRoot]) -> SkillScan {
    let mut result = SkillScan::default();
    let mut seen = HashSet::new();
    for root in roots {
        scan_root(root, &root.path, true, &mut seen, &mut result);
    }
    result.skills.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
    result
}

fn scan_root(
    root: &SkillRoot,
    directory: &Path,
    include_system: bool,
    seen: &mut HashSet<PathBuf>,
    result: &mut SkillScan,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            result
                .errors
                .push(format!("{}: {error}", directory.display()));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result
                    .errors
                    .push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            if include_system && path.file_name().is_some_and(|name| name == ".system") {
                scan_root(root, &path, false, seen, result);
                continue;
            }
            add_skill(root, path.join("SKILL.md"), seen, result);
        } else if root.flat_markdown && path.extension().is_some_and(|extension| extension == "md")
        {
            add_skill(root, path, seen, result);
        }
    }
}

fn add_skill(root: &SkillRoot, path: PathBuf, seen: &mut HashSet<PathBuf>, result: &mut SkillScan) {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            result.errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    if !canonical.is_file() {
        return;
    }
    if !seen.insert(canonical) {
        return;
    }
    let text = match read_limited(&path, METADATA_LIMIT) {
        Ok(text) => text,
        Err(error) => {
            result.errors.push(format!("{error:#}"));
            return;
        }
    };
    let fallback_name = if path.file_name().is_some_and(|name| name == "SKILL.md") {
        path.parent().and_then(Path::file_name)
    } else {
        path.file_stem()
    }
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| "Unnamed skill".to_string());
    let (frontmatter, body) = parse_frontmatter(&text);
    let name = frontmatter
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback_name);
    let description = frontmatter
        .description
        .filter(|description| !description.trim().is_empty())
        .unwrap_or_else(|| fallback_description(body));
    result.skills.push(SkillSummary {
        name,
        description: description.split_whitespace().collect::<Vec<_>>().join(" "),
        path,
        origin: root.origin,
        scope: root.scope,
    });
}

pub fn filter_skills(skills: &[SkillSummary], query: &str) -> Vec<usize> {
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return (0..skills.len()).collect();
    }

    let mut matches = skills
        .iter()
        .enumerate()
        .filter_map(|(index, skill)| {
            let name = skill.name.to_lowercase();
            let description = skill.description.to_lowercase();
            let mut name_terms = 0;
            let mut score = 0;
            for term in &terms {
                if let Some(name_score) = score_name(&name, term) {
                    name_terms += 1;
                    score += name_score;
                } else if let Some(position) = description.find(term) {
                    score += 1_000 - position.min(500) as i32;
                } else {
                    return None;
                }
            }
            Some((index, name_terms, score))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| {
                skills[left.0]
                    .name
                    .to_lowercase()
                    .cmp(&skills[right.0].name.to_lowercase())
            })
            .then_with(|| skills[left.0].path.cmp(&skills[right.0].path))
    });
    matches.into_iter().map(|(index, _, _)| index).collect()
}

fn score_name(name: &str, term: &str) -> Option<i32> {
    if name.starts_with(term) {
        return Some(4_000);
    }
    if let Some(position) = name.find(term) {
        return Some(3_000 - position.min(500) as i32);
    }

    let mut expected = term.chars();
    let mut next = expected.next()?;
    let mut previous = None;
    let mut gaps = 0;
    let mut start = 0;
    for (index, character) in name.chars().enumerate() {
        if character != next {
            continue;
        }
        if let Some(previous) = previous {
            gaps += index - previous - 1;
        } else {
            start = index;
        }
        previous = Some(index);
        match expected.next() {
            Some(character) => next = character,
            None => return Some((2_000 - (start + gaps * 4).min(1_000) as i32).max(1)),
        }
    }
    None
}

pub fn read_detail(path: &Path) -> Result<String> {
    let text = read_limited(path, DETAIL_LIMIT)?;
    let (_, body) = parse_frontmatter(&text);
    Ok(body.trim_start().to_string())
}

pub fn read_bundle(path: &Path) -> Result<SkillBundle> {
    let root = path
        .parent()
        .context("Skill has no parent directory")?
        .canonicalize()
        .with_context(|| format!("failed to locate skill directory for {}", path.display()))?;
    let entry_file = path
        .canonicalize()
        .with_context(|| format!("failed to locate {}", path.display()))?;
    if !entry_file.starts_with(&root) {
        bail!("Skill file is outside its directory: {}", path.display());
    }
    let mut bundle = SkillBundle {
        root: root.clone(),
        entry_file,
        files: Vec::new(),
        truncated: false,
    };
    let mut visited = HashSet::new();
    let flat = path.file_name().is_none_or(|name| name != "SKILL.md");
    scan_skill_directory(&root, &root, 0, flat, &mut visited, &mut bundle)?;
    if bundle.file(&bundle.entry_file).is_none() {
        bail!("Skill file is not readable: {}", path.display());
    }
    Ok(bundle)
}

fn scan_skill_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    flat: bool,
    visited: &mut HashSet<PathBuf>,
    bundle: &mut SkillBundle,
) -> Result<()> {
    if depth > TREE_DEPTH_LIMIT || !visited.insert(directory.canonicalize()?) {
        return Ok(());
    }
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        let left_path = left.path();
        let right_path = right.path();
        let left_name = left.file_name().to_string_lossy().to_lowercase();
        let right_name = right.file_name().to_string_lossy().to_lowercase();
        let first_name = bundle
            .entry_file
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        (left_name != first_name, !left_path.is_dir(), left_name).cmp(&(
            right_name != first_name,
            !right_path.is_dir(),
            right_name,
        ))
    });
    for entry in entries {
        if bundle.files.len() >= TREE_ENTRY_LIMIT {
            bundle.truncated = true;
            break;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "node_modules" | "target" | "__pycache__" | "venv"
            )
        {
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(path) if path.starts_with(root) => path,
            _ => continue,
        };
        let relative = path.strip_prefix(root)?.to_path_buf();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            if depth < TREE_DEPTH_LIMIT && !visited.contains(&canonical) {
                bundle.files.push(SkillFileEntry {
                    path: canonical.clone(),
                    relative,
                    depth,
                    kind: None,
                });
                scan_skill_directory(root, &path, depth + 1, flat, visited, bundle)?;
            } else if depth >= TREE_DEPTH_LIMIT {
                bundle.truncated = true;
            }
        } else if metadata.is_file() {
            if flat
                && depth == 0
                && path.extension().is_some_and(|ext| ext == "md")
                && canonical != bundle.entry_file
            {
                continue;
            }
            if let Some(kind) = classify_skill_file(&path, &metadata) {
                bundle.files.push(SkillFileEntry {
                    path: canonical,
                    relative,
                    depth,
                    kind: Some(kind),
                });
            }
        }
    }
    Ok(())
}

fn classify_skill_file(path: &Path, metadata: &fs::Metadata) -> Option<SkillFileKind> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif"
    ) {
        return (metadata.len() <= IMAGE_LIMIT).then_some(SkillFileKind::Image);
    }
    let mut file = File::open(path).ok()?;
    let mut probe = [0_u8; 1024];
    let count = file.read(&mut probe).ok()?;
    let sample = &probe[..count];
    if sample.starts_with(b"%PDF-")
        || sample
            .iter()
            .any(|byte| *byte < 0x20 && !matches!(*byte, b'\n' | b'\r' | b'\t'))
        || std::str::from_utf8(&probe[..count]).is_err_and(|error| error.error_len().is_some())
    {
        return None;
    }
    if extension == "md" || extension == "markdown" {
        return Some(SkillFileKind::Markdown);
    }
    let language = match extension.as_str() {
        "sh" | "bash" | "zsh" => Some("bash"),
        "go" => Some("go"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "rs" => Some("rust"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "json" | "jsonc" => Some("json"),
        _ => std::str::from_utf8(sample)
            .ok()
            .and_then(|text| text.lines().next())
            .filter(|line| line.starts_with("#!"))
            .and_then(|line| {
                if line.contains("python") {
                    Some("python")
                } else if line.contains("node") {
                    Some("javascript")
                } else if line.contains("ruby") {
                    Some("ruby")
                } else if line.contains("bash") || line.contains("/sh") {
                    Some("bash")
                } else {
                    None
                }
            }),
    };
    Some(SkillFileKind::Code(language))
}

pub fn read_skill_file(bundle: &SkillBundle, path: &Path) -> Result<Option<String>> {
    let entry = bundle.file(path).context("File is not in this skill")?;
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to locate {}", path.display()))?;
    if canonical != entry.path || !canonical.starts_with(&bundle.root) {
        bail!("File changed or moved outside the skill directory");
    }
    match entry.kind {
        Some(SkillFileKind::Image) => Ok(None),
        Some(SkillFileKind::Markdown) if path == bundle.entry_file => read_detail(path).map(Some),
        Some(SkillFileKind::Markdown | SkillFileKind::Code(_)) => {
            let metadata = fs::metadata(path)?;
            if metadata.len() > DETAIL_LIMIT {
                bail!("Skill file is larger than 2 MiB: {}", path.display());
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            File::open(path)?
                .take(DETAIL_LIMIT + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if bytes.len() as u64 > DETAIL_LIMIT {
                bail!("Skill file is larger than 2 MiB: {}", path.display());
            }
            String::from_utf8(bytes)
                .with_context(|| format!("Skill file is not UTF-8: {}", path.display()))
                .map(Some)
        }
        None => bail!("Not a file: {}", path.display()),
    }
}

pub fn resolve_skill_link(bundle: &SkillBundle, current: &Path, href: &str) -> SkillLink {
    let url = match Url::parse(href) {
        Ok(url) => url,
        Err(_) => match Url::from_file_path(current)
            .ok()
            .and_then(|base| base.join(href).ok())
        {
            Some(url) => url,
            None => return SkillLink::Unsupported,
        },
    };
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        return SkillLink::External(url.to_string());
    }
    if url.scheme() != "file" {
        return SkillLink::Unsupported;
    }
    let Some(path) = url
        .to_file_path()
        .ok()
        .and_then(|path| path.canonicalize().ok())
    else {
        return SkillLink::Unsupported;
    };
    if !path.starts_with(&bundle.root) || bundle.file(&path).is_none() {
        return SkillLink::Unsupported;
    }
    let anchor = url.fragment().map(|fragment| {
        percent_decode_str(fragment)
            .decode_utf8_lossy()
            .into_owned()
    });
    SkillLink::Local { path, anchor }
}

pub fn heading_for_anchor(markdown: &str, anchor: &str) -> Option<String> {
    let wanted = anchor.strip_prefix("user-content-").unwrap_or(anchor);
    let mut duplicates = std::collections::HashMap::<String, usize>::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let markers = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if !(1..=6).contains(&markers)
            || !trimmed
                .as_bytes()
                .get(markers)
                .is_some_and(u8::is_ascii_whitespace)
        {
            continue;
        }
        let heading = trimmed[markers..].trim().trim_end_matches('#').trim();
        let slug = heading
            .chars()
            .flat_map(char::to_lowercase)
            .filter_map(|character| {
                if character.is_alphanumeric() || character == '_' || character == '-' {
                    Some(character)
                } else if character.is_whitespace() {
                    Some('-')
                } else {
                    None
                }
            })
            .collect::<String>();
        let duplicate = duplicates.entry(slug.clone()).or_default();
        let slug = if *duplicate == 0 {
            slug
        } else {
            format!("{slug}-{duplicate}")
        };
        *duplicate += 1;
        if slug == wanted {
            return Some(heading.to_string());
        }
    }
    None
}

fn read_limited(path: &Path, limit: u64) -> Result<String> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("Not a regular skill file: {}", path.display());
    }
    if limit == DETAIL_LIMIT && metadata.len() > limit {
        bail!("Skill file is larger than 2 MiB: {}", path.display());
    }
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if limit == DETAIL_LIMIT && bytes.len() as u64 > limit {
        bail!("Skill file is larger than 2 MiB: {}", path.display());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_frontmatter(text: &str) -> (Frontmatter, &str) {
    let Some(after_opening) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (Frontmatter::default(), text);
    };
    let mut yaml_len = 0;
    for line in after_opening.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let frontmatter = serde_yaml::from_str(&after_opening[..yaml_len]).unwrap_or_default();
            return (frontmatter, &after_opening[yaml_len + line.len()..]);
        }
        yaml_len += line.len();
    }
    (Frontmatter::default(), text)
}

fn fallback_description(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && *line != "---")
        .unwrap_or("No description provided.")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        discover, filter_skills, heading_for_anchor, read_bundle, read_detail, read_skill_file,
        resolve_skill_link, roots, SkillAgent, SkillFileKind, SkillLink, SkillScope, SkillSummary,
    };

    fn sample(name: &str, description: &str) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            description: description.to_string(),
            path: std::path::PathBuf::from(name),
            origin: "Test",
            scope: SkillScope::User,
        }
    }

    #[test]
    fn ranks_name_prefix_contiguous_subsequence_then_description() {
        let skills = [
            sample("alpha-tool", ""),
            sample("toolbox", ""),
            sample("t-o-o-l", ""),
            sample("other", "tool support"),
        ];
        assert_eq!(filter_skills(&skills, "tool"), [1, 0, 2, 3]);
    }

    #[test]
    fn requires_all_terms_and_matches_unicode_without_case() {
        let skills = [
            sample("Byted-Aeolus", "查询 数据集"),
            sample("Byted-APM", "监控服务"),
            sample("other", "aeolus dataset"),
        ];
        assert_eq!(filter_skills(&skills, "BYTED 数据集"), [0]);
        assert_eq!(filter_skills(&skills, "btd aeolus"), [0]);
        assert_eq!(filter_skills(&skills, "  "), [0, 1, 2]);
    }

    #[test]
    fn discovers_frontmatter_from_user_and_project_roots() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let user_skill = home.path().join(".claude/skills/review");
        let project_skill = project.path().join(".claude/skills/deploy");
        fs::create_dir_all(&user_skill).unwrap();
        fs::create_dir_all(&project_skill).unwrap();
        fs::write(
            user_skill.join("SKILL.md"),
            "---\nname: Review\ndescription: >-\n  Check code\n  before merging.\n---\n# Review\n",
        )
        .unwrap();
        fs::write(project_skill.join("SKILL.md"), "# Deploy\nRun the release.").unwrap();

        let result = discover(&roots(
            SkillAgent::Claude,
            home.path(),
            Some(project.path()),
        ));
        assert!(result.errors.is_empty());
        assert_eq!(result.skills.len(), 2);
        assert!(result.skills.iter().any(
            |skill| skill.name == "Review" && skill.description == "Check code before merging."
        ));
        assert!(result
            .skills
            .iter()
            .any(|skill| skill.name == "deploy" && skill.description == "Run the release."));
    }

    #[test]
    fn missing_agent_directory_is_an_empty_catalog() {
        let home = tempdir().unwrap();
        let result = discover(&roots(SkillAgent::ZCode, home.path(), None));
        assert!(result.skills.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn grok_catalog_includes_native_shared_and_project_command_roots() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        for (root, relative, body) in [
            (
                home.path(),
                ".grok/bundled/skills/builtin/SKILL.md",
                "---\nname: grok-builtin\n---\n# Built-in",
            ),
            (
                home.path(),
                ".grok/skills/native/SKILL.md",
                "---\nname: grok-native\ndescription: Native Grok skill\n---\n# Native",
            ),
            (
                home.path(),
                ".claude/skills/compat/SKILL.md",
                "---\nname: claude-compat\n---\n# Compat",
            ),
            (
                project.path(),
                ".grok/commands/local.md",
                "---\nname: local-command\n---\n# Command",
            ),
        ] {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let result = discover(&roots(SkillAgent::Grok, home.path(), Some(project.path())));
        assert!(result.errors.is_empty());
        let names = result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert!(names.contains("grok-native"));
        assert!(names.contains("grok-builtin"));
        assert!(names.contains("claude-compat"));
        assert!(names.contains("local-command"));
    }

    #[test]
    fn agent_roots_include_their_user_and_project_locations() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let home_path = home.path();
        let project_path = project.path();

        for (agent, user, local) in [
            (SkillAgent::Claude, ".claude/skills", ".claude/skills"),
            (SkillAgent::Codex, ".codex/skills", ".agents/skills"),
            (SkillAgent::Grok, ".grok/skills", ".grok/skills"),
            (SkillAgent::Kimi, ".kimi/skills", ".kimi/skills"),
            (SkillAgent::ZCode, ".zcode/skills", ".zcode/skills"),
            (SkillAgent::Trae, ".trae-cn/skills", ".trae/skills"),
        ] {
            let found = roots(agent, home_path, Some(project_path));
            assert!(found.iter().any(|root| root.path == home_path.join(user)));
            assert!(found
                .iter()
                .any(|root| root.path == project_path.join(local)));
        }
        let kimi = roots(SkillAgent::Kimi, home_path, None);
        assert!(kimi
            .iter()
            .any(|root| root.path == home_path.join(".claude/skills")));
        assert!(kimi
            .iter()
            .any(|root| root.path == home_path.join(".codex/skills")));
    }

    #[test]
    fn limits_detail_read_without_loading_an_unbounded_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("SKILL.md");
        fs::write(&path, "x".repeat(2 * 1024 * 1024 + 1)).unwrap();
        assert!(read_detail(&path).is_err());
    }

    #[test]
    fn detail_renders_instructions_without_frontmatter() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("SKILL.md");
        fs::write(
            &path,
            "---\nname: Review\nallowed-tools: Bash\n---\n\n# Instructions\nRead code.",
        )
        .unwrap();
        assert_eq!(read_detail(&path).unwrap(), "# Instructions\nRead code.");
    }

    #[test]
    fn skill_bundle_lists_nested_text_and_images_but_not_binary_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("references")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("SKILL.md"), "---\nname: Test\n---\n# Guide\n").unwrap();
        fs::write(root.join("references/guide.md"), "# Reference\n").unwrap();
        fs::write(
            root.join("scripts/run.py"),
            "#!/usr/bin/env python3\nprint('ok')\n",
        )
        .unwrap();
        fs::write(
            root.join("scripts/launch"),
            "#!/usr/bin/env ruby\nputs 'ok'\n",
        )
        .unwrap();
        fs::write(root.join("scripts/tool"), [0x7f, b'E', b'L', b'F', 0]).unwrap();
        fs::write(root.join("notes.pdf"), b"%PDF-1.7\ntext only").unwrap();
        fs::write(root.join("diagram.png"), [137, 80, 78, 71]).unwrap();

        let bundle = read_bundle(&root.join("SKILL.md")).unwrap();
        assert!(bundle
            .file(&root.join("SKILL.md").canonicalize().unwrap())
            .is_some());
        assert!(bundle
            .file(&root.join("references/guide.md").canonicalize().unwrap())
            .is_some());
        assert_eq!(
            bundle
                .file(&root.join("scripts/run.py").canonicalize().unwrap())
                .unwrap()
                .kind,
            Some(SkillFileKind::Code(Some("python")))
        );
        assert_eq!(
            bundle
                .file(&root.join("scripts/launch").canonicalize().unwrap())
                .unwrap()
                .kind,
            Some(SkillFileKind::Code(Some("ruby")))
        );
        assert_eq!(
            bundle
                .file(&root.join("diagram.png").canonicalize().unwrap())
                .unwrap()
                .kind,
            Some(SkillFileKind::Image)
        );
        assert!(bundle
            .file(&root.join("scripts/tool").canonicalize().unwrap())
            .is_none());
        assert!(bundle
            .file(&root.join("notes.pdf").canonicalize().unwrap())
            .is_none());
        let collapsed = [std::path::PathBuf::from("scripts")].into_iter().collect();
        let visible = bundle.visible_indices(&collapsed);
        assert!(visible
            .iter()
            .any(|index| bundle.files[*index].relative == std::path::Path::new("scripts")));
        assert!(visible
            .iter()
            .all(|index| !bundle.files[*index].relative.starts_with("scripts/run.py")));
        assert!(bundle
            .visible_indices(&Default::default())
            .iter()
            .any(|index| bundle.files[*index].relative == std::path::Path::new("scripts/run.py")));
        assert_eq!(
            read_skill_file(&bundle, &bundle.entry_file).unwrap(),
            Some("# Guide\n".to_string())
        );
        assert!(matches!(
            resolve_skill_link(&bundle, &bundle.entry_file, "references/guide.md#reference"),
            SkillLink::Local { path, anchor }
                if path == root.join("references/guide.md").canonicalize().unwrap() && anchor.as_deref() == Some("reference")
        ));
        assert!(matches!(
            resolve_skill_link(&bundle, &bundle.entry_file, "diagram.png"),
            SkillLink::Local { path, .. } if path == root.join("diagram.png").canonicalize().unwrap()
        ));
        assert!(matches!(
            resolve_skill_link(&bundle, &bundle.entry_file, "#guide"),
            SkillLink::Local { path, anchor }
                if path == bundle.entry_file && anchor.as_deref() == Some("guide")
        ));
        assert!(matches!(
            resolve_skill_link(&bundle, &bundle.entry_file, "../../outside.md"),
            SkillLink::Unsupported
        ));
    }

    #[test]
    fn flat_skill_does_not_list_sibling_skills() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("one.md"), "# One").unwrap();
        fs::write(directory.path().join("two.md"), "# Two").unwrap();
        let bundle = read_bundle(&directory.path().join("one.md")).unwrap();
        assert_eq!(bundle.files.len(), 1);
        assert!(bundle.file(&bundle.entry_file).is_some());
        assert!(bundle.file(&directory.path().join("two.md")).is_none());
    }

    #[test]
    fn heading_anchors_handle_unicode_and_duplicates() {
        let markdown = "# Install Sail\n## 中文 用法\n## 中文 用法\n";
        assert_eq!(
            heading_for_anchor(markdown, "install-sail"),
            Some("Install Sail".into())
        );
        assert_eq!(
            heading_for_anchor(markdown, "中文-用法"),
            Some("中文 用法".into())
        );
        assert_eq!(
            heading_for_anchor(markdown, "中文-用法-1"),
            Some("中文 用法".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn skill_bundle_does_not_follow_symlinks_outside_root() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(directory.path().join("SKILL.md"), "# Safe").unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            directory.path().join("linked.txt"),
        )
        .unwrap();
        symlink(outside.path(), directory.path().join("linked-dir")).unwrap();

        let bundle = read_bundle(&directory.path().join("SKILL.md")).unwrap();
        assert!(bundle.file(&directory.path().join("linked.txt")).is_none());
        assert!(bundle
            .files
            .iter()
            .all(|entry| !entry.relative.starts_with("linked-dir")));
        assert!(matches!(
            resolve_skill_link(&bundle, &bundle.entry_file, "linked.txt"),
            SkillLink::Unsupported
        ));
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinked_skill_directories_once() {
        use std::os::unix::fs::symlink;

        let home = tempdir().unwrap();
        let shared = home.path().join(".agents/skills/shared");
        let codex = home.path().join(".codex/skills");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::write(shared.join("SKILL.md"), "---\nname: Shared\n---\nBody").unwrap();
        symlink(&shared, codex.join("shared")).unwrap();

        let result = discover(&roots(SkillAgent::Codex, home.path(), None));
        assert_eq!(result.skills.len(), 1);
    }
}
