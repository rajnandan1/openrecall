use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RELEASES: &str = "https://github.com/rajnandan1/openrecall/releases";
const ASSET: &str = "openrecall-aarch64-apple-darwin";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 24 * 60 * 60;
const FAILURES_BEFORE_MESSAGE: u32 = 3;

/// Only the release workflow builds with `OPENRECALL_RELEASE=1`; every other binary is a source build.
pub fn release() -> bool {
    option_env!("OPENRECALL_RELEASE") == Some("1")
}

/// A release binary with the off switch unset runs the update check.
pub fn on() -> bool {
    release() && std::env::var("OPENRECALL_UPDATE").as_deref() != Ok("0")
}

/// The output of `openrecall version`.
pub fn version() -> String {
    if release() {
        VERSION.to_string()
    } else {
        format!("{VERSION} (source build)")
    }
}

/// `MAJOR.MINOR.PATCH` as three integers; anything else does not parse.
pub fn parse(v: &str) -> Option<[u64; 3]> {
    let parts: Vec<u64> = v
        .split('.')
        .map(|n| {
            if n.bytes().all(|b| b.is_ascii_digit()) {
                n.parse().ok()
            } else {
                None
            }
        })
        .collect::<Option<_>>()?;
    parts.try_into().ok()
}

/// `update/state.json` in the home.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    attempted_at: u64,
    failures: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plugin: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
}

impl State {
    fn record(&mut self, attempt: &Attempt) {
        match attempt {
            Attempt::Swapped(to) => {
                self.failures = 0;
                self.message = format!(
                    "OpenRecall updated from {VERSION} to {to}. Sessions that were open before the update get the new MCP server and plugin after a restart of Claude Code."
                );
            }
            Attempt::Current(_) => self.failures = 0,
            Attempt::Failed(why) => {
                self.failures += 1;
                if self.failures >= FAILURES_BEFORE_MESSAGE {
                    self.message = format!(
                        "OpenRecall could not update itself: {why}. To update by hand, run: {}",
                        crate::INSTALL
                    );
                }
            }
        }
    }
}

fn dir() -> PathBuf {
    crate::home().join("update")
}

pub fn load() -> State {
    fs::read_to_string(dir().join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(state: &State) -> crate::Result<()> {
    Ok(crate::write_atomic(
        &dir().join("state.json"),
        &serde_json::to_string(state)?,
    )?)
}

/// The update lock, held until the file drops; None while another process holds it.
fn lock() -> Option<fs::File> {
    fs::create_dir_all(dir()).ok()?;
    let file = fs::File::create(dir().join("lock")).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// The message that waits for a SessionStart, removed once taken. It waits on while a job holds the lock.
pub fn take_message() -> Option<String> {
    if load().message.is_empty() {
        return None;
    }
    let _lock = lock()?;
    let mut state = load();
    let message = std::mem::take(&mut state.message);
    save(&state).ok()?;
    Some(message).filter(|m| !m.is_empty())
}

/// The version of the session's plugin, from the manifest that `CLAUDE_PLUGIN_ROOT` points at.
pub fn session_plugin() -> Option<String> {
    let root = std::env::var_os("CLAUDE_PLUGIN_ROOT")?;
    let manifest = fs::read_to_string(Path::new(&root).join(".claude-plugin/plugin.json")).ok()?;
    let manifest: Value = serde_json::from_str(&manifest).ok()?;
    manifest["version"].as_str().map(str::to_string)
}

/// The version warning, only where the user may need to act.
pub fn version_mismatch() -> Option<String> {
    warning(&session_plugin()?, VERSION, &load().plugin, on())
}

fn warning(
    plugin: &str,
    binary: &str,
    last_plugin_step: &str,
    update_check_runs: bool,
) -> Option<String> {
    if parse(plugin)? > parse(binary)? {
        (!update_check_runs).then(|| {
            format!(
                "OpenRecall binary {binary} is older than plugin {plugin}. Update the binary: {}",
                crate::INSTALL
            )
        })
    } else if plugin_due(plugin, last_plugin_step, binary) {
        Some(if update_check_runs {
            format!(
                "OpenRecall plugin {plugin} is older than binary {binary}. OpenRecall updates the plugin in the background now. If you see this message again, run: claude plugin update openrecall@openrecall"
            )
        } else {
            format!(
                "OpenRecall plugin {plugin} is older than binary {binary}. Update the plugin: claude plugin update openrecall@openrecall. Then restart Claude Code."
            )
        })
    } else {
        None
    }
}

/// `openrecall update --job`: the binary step at most once in 24 hours, then the plugin step, under one lock.
pub fn job() -> crate::Result<()> {
    if !on() {
        return Ok(());
    }
    let Some(_lock) = lock() else {
        return Ok(());
    };
    let target = fs::canonicalize(std::env::current_exe()?)?;
    remove_leftovers(&target);
    let mut state = load();
    let mut on_disk = VERSION.to_string();
    let now = crate::now_secs();
    if now.abs_diff(state.attempted_at) >= DAY {
        state.attempted_at = now;
        save(&state)?;
        let attempt = binary_step(RELEASES, &target);
        state.record(&attempt);
        save(&state)?;
        match attempt {
            Attempt::Swapped(to) => {
                log("binary", "updated", VERSION, &to, "");
                on_disk = to;
            }
            Attempt::Current(why) => log("binary", "current", VERSION, "", &why),
            Attempt::Failed(why) => log("binary", "failed", VERSION, "", &why),
        }
    }
    if let Some(session) = session_plugin()
        && plugin_due(&session, &state.plugin, &on_disk)
    {
        match plugin_step() {
            Ok(()) => {
                state.plugin = on_disk.clone();
                save(&state)?;
                log("plugin", "updated", &session, &on_disk, "");
            }
            Err(why) => log("plugin", "failed", &session, &on_disk, &why),
        }
    }
    Ok(())
}

fn log(step: &str, result: &str, from: &str, to: &str, reason: &str) {
    crate::log(
        json!({"event": "update", "step": step, "result": result, "from": from, "to": to, "reason": reason}),
    );
}

/// The temporary files that a killed job left beside the binary.
fn remove_leftovers(target: &Path) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let prefix = format!("{}.tmp", name.to_string_lossy());
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// An old session still holds the old plugin, so the last successful plugin step must lag the binary too.
fn plugin_due(session: &str, done: &str, on_disk: &str) -> bool {
    let disk = parse(on_disk);
    parse(session).is_some_and(|s| Some(s) < disk) && parse(done) < disk
}

/// Only "updated" proves an update: two updates that collide, or one within 30 s of a refresh, report "up_to_date"
/// and leave the plugin old.
fn plugin_step() -> Result<(), String> {
    let (ok, out) = run_for(
        Command::new("claude").args(["plugin", "update", "openrecall@openrecall", "--json"]),
        Duration::from_secs(60),
    )?;
    let reply = out
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .unwrap_or_default();
    if ok
        && reply["updateOutcome"] == "updated"
        && reply["refreshFailed"] != true
        && reply["failureCode"].is_null()
    {
        Ok(())
    } else {
        Err(format!(
            "claude plugin update (exit ok: {ok}): {}",
            out.trim()
        ))
    }
}

#[derive(Debug, PartialEq)]
enum Attempt {
    Swapped(String),
    Current(String),
    Failed(String),
}

/// One attempt: the tag of the latest release, then its binary downloaded, checked and swapped in over `target`.
fn binary_step(releases: &str, target: &Path) -> Attempt {
    let latest = format!("{releases}/latest");
    let to = match curl(
        releases,
        10,
        &["-o", "/dev/null", "-w", "%{redirect_url}", &latest],
        Stdio::piped(),
    ) {
        Ok(to) if !to.is_empty() => to,
        Ok(_) => return Attempt::Failed(format!("no redirect from {latest}")),
        Err(why) => return Attempt::Failed(why),
    };
    let tag = to
        .rsplit_once("/releases/tag/v")
        .map(|(_, v)| v)
        .filter(|v| parse(v).is_some());
    let Some(version) = tag else {
        return Attempt::Current(format!("no release tag in {to:?}"));
    };
    if parse(version) <= parse(VERSION) {
        return Attempt::Current(format!("latest is {version}"));
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = target.with_file_name(format!("{name}.tmp{}", std::process::id()));
    let swapped = fetch(releases, version, &tmp)
        .and_then(|()| fs::rename(&tmp, target).map_err(|e| format!("swap: {e}")));
    match swapped {
        Ok(()) => Attempt::Swapped(version.to_string()),
        Err(why) => {
            let _ = fs::remove_file(&tmp);
            Attempt::Failed(why)
        }
    }
}

/// The hash file and the binary of one tag, so both come from one release; the binary lands in `tmp`, executable,
/// and must pass the integrity check and the smoke test.
fn fetch(releases: &str, version: &str, tmp: &Path) -> Result<(), String> {
    let url = format!("{releases}/download/v{version}/{ASSET}");
    let sum = curl(
        releases,
        10,
        &["-fL", &format!("{url}.sha256")],
        Stdio::piped(),
    )?;
    let want = sum
        .get(..64)
        .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "the hash file holds no hash".to_string())?
        .to_ascii_lowercase();
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(tmp)
        .map_err(|e| format!("temporary file: {e}"))?;
    curl(releases, 120, &["-fL", &url], file.into())?;
    let got = sha256(&fs::read(tmp).map_err(|e| format!("temporary file: {e}"))?);
    if got != want {
        return Err(format!(
            "hash mismatch: the download hashes to {got}, the release says {want}"
        ));
    }
    // main() exits 0 on every path, so only the output proves that the download runs.
    let (_, out) = run_for(Command::new(tmp).arg("version"), Duration::from_secs(5))?;
    if out.trim() != version {
        return Err(format!(
            "smoke test: the download printed {:?}, not {version}",
            out.trim()
        ));
    }
    Ok(())
}

/// `/usr/bin/curl` limited to the scheme of `releases`: HTTPS for a release binary, plain HTTP for a test server.
fn curl(releases: &str, limit: u64, args: &[&str], out: Stdio) -> Result<String, String> {
    let scheme = releases.split(':').next().unwrap_or("https");
    let done = Command::new("/usr/bin/curl")
        .args([
            "-q",
            "-sS",
            "--proto",
            &format!("={scheme}"),
            "--max-time",
            &limit.to_string(),
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(out)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !done.status.success() {
        let code = done.status.code().unwrap_or(-1);
        return Err(format!(
            "{} (curl exit {code})",
            String::from_utf8_lossy(&done.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

/// The command's success and stdout, or why it did not finish within `limit`. A thread drains stdout, so a full pipe
/// cannot stall the child, and a grandchild that keeps the pipe open cannot stall the caller.
fn run_for(cmd: &mut Command, limit: Duration) -> Result<(bool, String), String> {
    let started = Instant::now();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    while started.elapsed() < limit {
        if let Ok(Some(status)) = child.try_wait() {
            let text = rx
                .recv_timeout(limit.saturating_sub(started.elapsed()))
                .unwrap_or_default();
            return Ok((status.success(), text));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "{:?} ran past {} s",
        cmd.get_program(),
        limit.as_secs()
    ))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    /// GitHub's release addresses on localhost: `/latest` redirects to `latest` (an empty one gives a 503), and the two files of any tag
    /// answer with `sum` and `binary`. Returns the base address and the paths requested.
    fn serve(latest: &str, sum: &str, binary: &[u8]) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let kept = seen.clone();
        let location = format!("{origin}{latest}");
        let outage = latest.is_empty();
        let (sum, binary) = (sum.as_bytes().to_vec(), binary.to_vec());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let (head, body) = if path == "/releases/latest" && outage {
                    ("503 Service Unavailable".into(), &[][..])
                } else if path == "/releases/latest" {
                    (format!("302 Found\r\nLocation: {location}"), &[][..])
                } else if path.ends_with(".sha256") {
                    ("200 OK".into(), &sum[..])
                } else if path.ends_with(ASSET) {
                    ("200 OK".into(), &binary[..])
                } else {
                    ("404 Not Found".into(), &[][..])
                };
                write!(
                    stream,
                    "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
                kept.lock().unwrap().push(path);
            }
        });
        (format!("{origin}/releases"), seen)
    }

    #[test]
    fn a_message_waits_for_the_third_failure_and_the_plugin_step_for_both_versions() {
        let mut s = State::default();
        for _ in 0..2 {
            s.record(&Attempt::Failed("x".into()));
        }
        assert!(s.message.is_empty());
        s.record(&Attempt::Failed("no route".into()));
        assert_eq!(
            s.message,
            "OpenRecall could not update itself: no route. To update by hand, run: curl -fsSL https://raw.githubusercontent.com/rajnandan1/openrecall/main/install.sh | sh"
        );
        s.record(&Attempt::Current("latest is 0.0.1".into()));
        assert_eq!(s.failures, 0);
        s.record(&Attempt::Swapped("99.0.0".into()));
        assert!(s.message.starts_with(&format!(
            "OpenRecall updated from {VERSION} to 99.0.0. Sessions that were open"
        )));

        assert!(plugin_due("0.2.0", "", "0.3.0"));
        assert!(plugin_due("0.2.0", "0.2.0", "0.3.0"));
        assert!(
            !plugin_due("0.2.0", "0.3.0", "0.3.0"),
            "an old session after the plugin step"
        );
        assert!(!plugin_due("0.3.0", "", "0.3.0"));
        assert!(
            !plugin_due("0.4.0", "", "0.3.0"),
            "a newer plugin starts nothing"
        );
        assert!(!plugin_due("0.2", "", "0.3.0"));
    }

    #[test]
    fn the_version_warning_shows_only_where_the_user_may_act() {
        let w = warning;
        assert_eq!(
            w("0.3.0", "0.2.0", "", false).as_deref(),
            Some(
                "OpenRecall binary 0.2.0 is older than plugin 0.3.0. Update the binary: curl -fsSL https://raw.githubusercontent.com/rajnandan1/openrecall/main/install.sh | sh"
            )
        );
        assert_eq!(
            w("0.3.0", "0.2.0", "", true),
            None,
            "the binary step repairs it"
        );
        assert!(
            w("0.2.0", "0.3.0", "0.2.0", true)
                .unwrap()
                .contains("updates the plugin in the background now")
        );
        assert!(
            w("0.2.0", "0.3.0", "", false)
                .unwrap()
                .ends_with("Update the plugin: claude plugin update openrecall@openrecall. Then restart Claude Code.")
        );
        assert_eq!(w("0.2.0", "0.3.0", "0.3.0", true), None, "a mixed session");
        assert_eq!(
            w("0.10.0", "0.9.0", "", true),
            None,
            "three integers, not text"
        );
        assert_eq!(w("0.2.0", "0.2.0", "", false), None);
        assert_eq!(w("0.2.0-beta", "0.3.0", "", false), None);
    }

    #[test]
    fn the_binary_step_swaps_in_only_a_checked_newer_release() {
        let dir = std::env::temp_dir().join(format!("openrecall-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("openrecall");
        let new = b"#!/bin/sh\necho 99.0.0\n";
        let sum = format!("{}  {ASSET}\n", sha256(new));
        let step = |latest: &str, sum: &str, binary: &[u8]| {
            fs::write(&target, "old").unwrap();
            let (base, seen) = serve(latest, sum, binary);
            let attempt = binary_step(&base, &target);
            let left: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(left, ["openrecall"], "no temporary file stays");
            let seen = seen.lock().unwrap().clone();
            (attempt, fs::read(&target).unwrap(), seen)
        };

        let (attempt, now, seen) = step("/releases/tag/v99.0.0", &sum, new);
        assert_eq!(attempt, Attempt::Swapped("99.0.0".into()));
        assert_eq!(now, new);
        assert_ne!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o111,
            0
        );
        let file = format!("/releases/download/v99.0.0/{ASSET}");
        assert_eq!(
            seen,
            [
                "/releases/latest".to_string(),
                format!("{file}.sha256"),
                file
            ]
        );

        for latest in [
            "/releases",
            "/releases/tag/v0.0.9",
            "/releases/tag/v99.0",
            "/releases/tag/nightly",
        ] {
            let (attempt, now, seen) = step(latest, &sum, new);
            assert!(
                matches!(attempt, Attempt::Current(_)),
                "{latest}: {attempt:?}"
            );
            assert_eq!((now.as_slice(), seen.len()), (&b"old"[..], 1), "{latest}");
        }

        let (attempt, now, _) = step("", &sum, new);
        assert!(
            matches!(&attempt, Attempt::Failed(r) if r.contains("no redirect")),
            "{attempt:?}"
        );
        assert_eq!(now, b"old");

        let liar = b"#!/bin/sh\necho 98.0.0\n";
        for (sum, binary, why) in [
            (
                format!("{}  {ASSET}\n", "0".repeat(64)),
                &new[..],
                "hash mismatch",
            ),
            ("<html>rate limited</html>".into(), &new[..], "no hash"),
            (
                format!("{}  {ASSET}\n", sha256(liar)),
                &liar[..],
                "smoke test",
            ),
        ] {
            let (attempt, now, _) = step("/releases/tag/v99.0.0", &sum, binary);
            assert!(
                matches!(&attempt, Attempt::Failed(r) if r.contains(why)),
                "{why}: {attempt:?}"
            );
            assert_eq!(now, b"old", "{why}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn versions_are_three_integers() {
        assert_eq!(parse("0.10.2"), Some([0, 10, 2]));
        assert!(parse("0.10.2") > parse("0.9.12"));
        for bad in [
            "",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc1",
            "1.+2.3",
            "v1.2.3",
            "1..3",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
