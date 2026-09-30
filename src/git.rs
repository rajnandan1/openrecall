use regex::Regex;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

/// The worktree around a hook's `cwd`, read from `.git` by hand (ticket 10).
pub struct Repo {
    /// The worktree root: the directory that holds `.git`.
    pub folder: PathBuf,
    gitdir: PathBuf,
    common: PathBuf,
    pub identity: String,
}

pub const TRUNK: [&str; 2] = ["main", "master"];

impl Repo {
    pub fn find(cwd: &str) -> Option<Repo> {
        let mut dir = Path::new(cwd);
        loop {
            let dotgit = dir.join(".git");
            if dotgit.is_dir() {
                return Repo::open(dir, dotgit.clone(), dotgit);
            }
            if dotgit.is_file() {
                let text = fs::read_to_string(&dotgit).ok()?;
                let gitdir = dir.join(text.trim().strip_prefix("gitdir:")?.trim());
                let common = match fs::read_to_string(gitdir.join("commondir")) {
                    Ok(rel) => gitdir.join(rel.trim()),
                    Err(_) => gitdir.clone(),
                };
                return Repo::open(dir, gitdir, common);
            }
            dir = dir.parent()?;
        }
    }

    fn open(folder: &Path, gitdir: PathBuf, common: PathBuf) -> Option<Repo> {
        let common = clean(&common);
        let identity = match origin(&common) {
            Some(url) => normalize(&url),
            None => format!("local{}", common.parent()?.display()),
        };
        let safe = !identity.is_empty()
            && Path::new(&identity)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        safe.then(|| Repo {
            folder: folder.to_path_buf(),
            gitdir,
            common,
            identity,
        })
    }

    /// The checked-out branch, or None on a detached HEAD.
    pub fn branch(&self) -> Option<String> {
        let head = fs::read_to_string(self.gitdir.join("HEAD")).ok()?;
        head.trim()
            .strip_prefix("ref: refs/heads/")
            .map(str::to_string)
    }

    pub fn has_branch(&self, name: &str) -> bool {
        self.common.join("refs/heads").join(name).is_file()
            || fs::read_to_string(self.common.join("packed-refs")).is_ok_and(|p| {
                p.lines().any(|l| {
                    l.split_once(' ')
                        .is_some_and(|(_, r)| r == format!("refs/heads/{name}"))
                })
            })
    }
}

pub fn is_task(branch: &str) -> bool {
    !branch.is_empty() && !TRUNK.contains(&branch)
}

fn origin(common: &Path) -> Option<String> {
    let config = fs::read_to_string(common.join("config")).ok();
    let parsed = config.as_deref().and_then(|c| {
        let section = c.split_once("[remote \"origin\"]")?.1;
        let section = section.split('[').next()?;
        section.lines().find_map(|l| {
            let (key, value) = l.split_once('=')?;
            (key.trim() == "url").then(|| value.trim().to_string())
        })
    });
    parsed.or_else(|| {
        let out = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(common)
            .output()
            .ok()?;
        let url = String::from_utf8(out.stdout).ok()?;
        (out.status.success() && !url.trim().is_empty()).then(|| url.trim().to_string())
    })
}

/// `git@github.com:Owner/Repo.git` and `https://github.com/owner/repo` both give `github.com/owner/repo`.
pub fn normalize(url: &str) -> String {
    static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w+.-]+://").unwrap());
    static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^@/]+@").unwrap());
    static PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^/:]+):(\d+/)?").unwrap());
    static TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\.git)?/*$").unwrap());
    let u = SCHEME.replace(url, "");
    let u = USER.replace(&u, "");
    let u = PORT.replace(&u, "$1/");
    TAIL.replace(&u, "").to_lowercase()
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_forms() {
        for url in [
            "git@github.com:someone/app.git",
            "https://github.com/someone/app",
            "ssh://git@github.com:22/someone/app/",
        ] {
            assert_eq!(normalize(url), "github.com/someone/app");
        }
    }

    #[test]
    fn worktree_and_refs() {
        let tmp = std::env::temp_dir().join(format!("openrecall-git-{}", std::process::id()));
        let main = tmp.join("main");
        let wt = tmp.join("wt");
        fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        fs::create_dir_all(main.join(".git/refs/heads/feat")).unwrap();
        fs::create_dir_all(wt.join("src/deep")).unwrap();
        fs::write(
            main.join(".git/config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:Someone/App.git\n",
        )
        .unwrap();
        fs::write(main.join(".git/refs/heads/feat/one"), "abc\n").unwrap();
        fs::write(
            main.join(".git/packed-refs"),
            "# pack-refs\nabc123 refs/heads/old\n",
        )
        .unwrap();
        fs::write(
            main.join(".git/worktrees/wt/HEAD"),
            "ref: refs/heads/feat/one\n",
        )
        .unwrap();
        fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();

        let repo = Repo::find(wt.join("src/deep").to_str().unwrap()).unwrap();
        assert_eq!(repo.folder, wt);
        assert_eq!(repo.identity, "github.com/someone/app");
        assert_eq!(repo.branch().as_deref(), Some("feat/one"));
        assert!(repo.has_branch("feat/one") && repo.has_branch("old") && !repo.has_branch("gone"));

        fs::write(main.join(".git/config"), "[core]\n").unwrap();
        let local = Repo::find(main.to_str().unwrap()).unwrap();
        assert_eq!(local.identity, format!("local{}", main.display()));
        fs::remove_dir_all(tmp).unwrap();
    }
}
