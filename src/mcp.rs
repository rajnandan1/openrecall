use crate::{git, index, record, scan};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServiceExt, schemars, tool, tool_router, transport::stdio};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Component, Path, PathBuf};

const DEFAULT_K: usize = 5;
const MAX_K: usize = 20;

#[derive(Deserialize, schemars::JsonSchema)]
struct RecallParams {
    /// Words to search for, or a memory's address to read that whole memory.
    query: String,
    /// How many memories to return, 1 to 20; 5 when left out.
    k: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RememberParams {
    /// The fact, in plain Markdown: what, why, when, and the identifiers it needs.
    text: String,
    /// decision, preference, pointer, state or gotcha.
    #[serde(rename = "type")]
    kind: String,
    /// `repo` for this repository, `global` for every repository.
    scope: String,
    /// A kebab-case file name, which becomes the memory's address; made from the text when left out.
    name: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ForgetParams {
    /// The memory's address, as a recalled line shows it.
    id: String,
}

/// The three tools of spec section 4, served over stdio (ticket 18: `openrecall mcp`).
#[derive(Clone)]
pub struct Server {
    home: PathBuf,
    user_home: PathBuf,
    cwd: String,
    session: String,
}

enum Kind {
    Fact,
    Builtin,
    Handoff,
}

#[tool_router(server_handler)]
impl Server {
    #[tool(
        description = "Search OpenRecall's memories for this repository and the global ones, or read one whole memory by its address. Use it for \"what did we decide about X\" and to expand a recalled line."
    )]
    fn recall(&self, Parameters(p): Parameters<RecallParams>) -> Result<String, String> {
        let query = p.query.trim();
        if let Some((path, _)) = file_of(query, &self.home, &self.user_home)
            && path.is_file()
        {
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            self.log("recall", json!({"address": query}));
            return Ok(format!("{}\n\n{text}", path.display()));
        }
        let repo = git::Repo::find(&self.cwd);
        let scopes = index::scopes(
            &self.home,
            &self.user_home,
            repo.as_ref().map(|r| r.identity.as_str()),
            repo.as_ref().and_then(git::Repo::main_checkout),
        );
        let mut ix = index::Index::open(&self.home).map_err(|e| e.to_string())?;
        ix.sync(&scopes).map_err(|e| e.to_string())?;
        let k = p.k.unwrap_or(DEFAULT_K).clamp(1, MAX_K);
        let found = ix
            .search(&index::terms(query), &scopes, k)
            .map_err(|e| e.to_string())?;
        let addresses: Vec<&str> = found.iter().map(|c| c.address.as_str()).collect();
        self.log(
            "recall",
            json!({"query_hash": crate::fnv(query), "addresses": addresses}),
        );
        if found.is_empty() {
            return Ok("No memory matches.".into());
        }
        Ok(found.iter().map(|c| index::line(c, 0)).collect::<Vec<_>>().join("\n"))
    }

    #[tool(
        description = "Store one fact for later sessions, as a Markdown memory file. Use it when the user says \"remember this\". A text that holds a secret is refused."
    )]
    fn remember(&self, Parameters(p): Parameters<RememberParams>) -> Result<String, String> {
        if !index::KINDS.contains(&p.kind.as_str()) {
            return Err(format!("type must be one of {}", index::KINDS.join(", ")));
        }
        let text = p.text.trim();
        if text.is_empty() {
            return Err("text is empty".into());
        }
        let (dir, scope) = match p.scope.as_str() {
            "global" => (self.home.join("global"), "global".to_string()),
            "repo" => {
                let repo =
                    git::Repo::find(&self.cwd).ok_or("no git repository here; use scope global")?;
                (self.home.join("repos").join(&repo.identity), repo.identity)
            }
            _ => return Err("scope must be repo or global".into()),
        };
        let (_, rules) = scan::Scanner::new().redact(text);
        if let Some(rule) = rules.first() {
            self.log("remember", json!({"dropped": rule}));
            return Err(format!(
                "not stored: the text holds a secret (gitleaks rule {rule}); rotate it"
            ));
        }
        let stem = index::free_stem(
            &dir,
            &index::name_of(
                p.name
                    .as_deref()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or(text),
            ),
        );
        let now = crate::now_ms();
        let file = index::render(&index::Memory {
            name: stem.clone(),
            description: description_of(text),
            expires: if p.kind == "state" {
                index::expires_from((now / 1000) as u64)
            } else {
                String::new()
            },
            kind: p.kind,
            scope: scope.clone(),
            source: format!("{} {}", self.session, record::iso_ms(now)),
            updated: String::new(),
            body: text.to_string(),
        });
        let path = dir.join(format!("{stem}.md"));
        crate::write_atomic(&path, &file).map_err(|e| e.to_string())?;
        let address = format!("{scope}/{stem}");
        self.log("remember", json!({"address": address}));
        Ok(format!("Stored as {address} ({})", path.display()))
    }

    #[tool(
        description = "Delete one of OpenRecall's own memories by its address. Claude Code's memory files and handoff records are not deleted here."
    )]
    fn forget(&self, Parameters(p): Parameters<ForgetParams>) -> Result<String, String> {
        let address = p.id.trim();
        let (path, kind) = file_of(address, &self.home, &self.user_home)
            .ok_or_else(|| format!("{address} is not a memory address"))?;
        match kind {
            Kind::Builtin => {
                return Err(format!(
                    "{address} is a Claude Code memory file, not OpenRecall's: edit or delete {} yourself",
                    path.display()
                ));
            }
            Kind::Handoff => {
                return Err(format!(
                    "{address} is a handoff record: it retires by itself after 7 idle days, or delete {} yourself",
                    path.display()
                ));
            }
            Kind::Fact => {}
        }
        if !path.is_file() {
            return Err(format!("no memory at {address}"));
        }
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let copies = replaced_copies(&path, &stem)
            .into_iter()
            .filter(|c| fs::remove_file(c).is_ok())
            .count();
        self.log("forget", json!({"address": address, "replaced": copies}));
        Ok(match copies {
            0 => format!("Forgot {address} ({})", path.display()),
            n => format!(
                "Forgot {address} ({}) and {n} replaced copies",
                path.display()
            ),
        })
    }

    fn log(&self, tool: &str, detail: Value) {
        let mut event = json!({"event": "mcp", "tool": tool, "session": self.session});
        if let (Some(e), Some(d)) = (event.as_object_mut(), detail.as_object()) {
            e.extend(d.clone());
        }
        crate::log(event);
    }
}

/// Runs until stdin closes (ticket 06), so the server dies with its session.
pub fn serve() -> crate::Result<()> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let server = Server {
        home: crate::home(),
        user_home: crate::user_home(),
        cwd: var("CLAUDE_PROJECT_DIR")
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|d| d.display().to_string())
            })
            .unwrap_or_default(),
        session: var("CLAUDE_CODE_SESSION_ID").unwrap_or_else(|| "mcp".into()),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            server.serve(stdio()).await?.waiting().await?;
            Ok(())
        })
}

/// Ticket 19: an address is a path under its root, so the file is found by joining strings. `None` for anything
/// that is not a plain relative path of the known shapes.
fn file_of(address: &str, home: &Path, user_home: &Path) -> Option<(PathBuf, Kind)> {
    let rel = Path::new(address);
    let plain = !address.is_empty()
        && !address.ends_with('/')
        && rel.components().all(|c| matches!(c, Component::Normal(_)));
    if !plain {
        return None;
    }
    let parts: Vec<&str> = address.split('/').collect();
    match parts.as_slice() {
        ["builtin", slug, stem] => Some((
            user_home
                .join(".claude/projects")
                .join(slug)
                .join("memory")
                .join(format!("{stem}.md")),
            Kind::Builtin,
        )),
        ["global", stem] => Some((home.join("global").join(format!("{stem}.md")), Kind::Fact)),
        [first, _, ..] if *first != "builtin" && *first != "global" => {
            let kind = if parts.contains(&"handoffs") {
                Kind::Handoff
            } else {
                Kind::Fact
            };
            Some((home.join("repos").join(format!("{address}.md")), kind))
        }
        _ => None,
    }
}

/// Ticket 26: a replaced fact's old texts sit beside it as `replaced/<stem>.<timestamp>.md`.
fn replaced_copies(path: &Path, stem: &str) -> Vec<PathBuf> {
    let dir = path.parent().map(|d| d.join("replaced"));
    fs::read_dir(dir.unwrap_or_default())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            name.strip_prefix(stem)
                .and_then(|r| r.strip_prefix('.'))
                .and_then(|r| r.strip_suffix(".md"))
                .is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        })
        .collect()
}

/// The first line, single-spaced and cut to the injection line's 200 characters.
fn description_of(text: &str) -> String {
    let first: Vec<&str> = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    let line = first.join(" ");
    if line.chars().count() <= 200 {
        line
    } else {
        line.chars().take(199).chain(['…']).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_map_to_files() {
        let (home, user) = (Path::new("/h"), Path::new("/u"));
        let at = |a: &str| {
            file_of(a, home, user)
                .map(|(p, k)| (p, matches!(k, Kind::Fact), matches!(k, Kind::Handoff)))
        };
        assert_eq!(
            at("global/x"),
            Some((PathBuf::from("/h/global/x.md"), true, false))
        );
        assert_eq!(
            at("github.com/a/b/x"),
            Some((PathBuf::from("/h/repos/github.com/a/b/x.md"), true, false))
        );
        assert_eq!(
            at("local/Users/me/app/x"),
            Some((
                PathBuf::from("/h/repos/local/Users/me/app/x.md"),
                true,
                false
            ))
        );
        assert_eq!(
            at("github.com/a/b/handoffs/feat--x"),
            Some((
                PathBuf::from("/h/repos/github.com/a/b/handoffs/feat--x.md"),
                false,
                true
            ))
        );
        assert_eq!(
            at("github.com/a/b/handoffs/retired/feat--x").map(|t| t.2),
            Some(true)
        );
        assert_eq!(
            at("builtin/-Users-me-app/x").map(|t| (t.0, t.1)),
            Some((
                PathBuf::from("/u/.claude/projects/-Users-me-app/memory/x.md"),
                false
            ))
        );
        for bad in [
            "",
            "x",
            "global/",
            "global/a/b",
            "builtin/s",
            "builtin/s/a/b",
            "../x",
            "/etc/passwd",
            "a/../b",
            "global/../x",
            "what did we decide",
        ] {
            assert!(file_of(bad, home, user).is_none(), "{bad}");
        }
    }

    #[test]
    fn descriptions() {
        assert_eq!(description_of("first  line\nsecond"), "first line");
        assert_eq!(description_of(&"x".repeat(300)).chars().count(), 200);
    }
}
