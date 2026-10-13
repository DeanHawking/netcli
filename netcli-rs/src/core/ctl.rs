//! Candidate/commit logic, formerly the shell script `uci-ctl`.
//!
//! `uci set/delete/changes/revert` already give the candidate-vs-running split
//! natively. This module adds what uci does not provide:
//!
//!   commit              snapshot /etc/config, `uci commit`, reload changed services
//!   commit-confirmed N  same, plus an automatic rollback unless `confirm` comes within N s
//!   confirm             cancel the pending automatic rollback
//!   rollback N          load the snapshot from N commits back into the CANDIDATE
//!                       (Junos model: nothing is applied until `commit`);
//!                       rollback 0 discards the candidate
//!   boot-recover        init-script hook: undo a commit-confirmed that was still
//!                       pending when the device went down
//!
//! A snapshot is a directory holding copies of the files of /etc/config (no
//! tar needed). All processes (uci, init scripts, logger) go through the
//! `Runner`, so everything here is unit-testable with a fake system.
//!
//! Two internal entry points exist for the binary itself (`--ctl-timer`,
//! `--boot-recover`, see `internal_command`): the detached rollback timer is
//! this same executable re-run in its own session.

use super::exec::{Output, Runner};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const KEEP_SNAPSHOTS: usize = 20;
const LOCK_WAIT_STEP: Duration = Duration::from_millis(100);
const LOCK_WAIT_TRIES: u32 = 100;

type R<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub struct Paths {
    pub snapshot_dir: PathBuf,
    pub etc_dir: PathBuf,
    /// Persistent (flash) marker of a pending commit-confirmed. It must
    /// survive a reboot: the new config is already on flash, and without the
    /// marker nothing would ever roll it back.
    pub pending_file: PathBuf,
    pub lock_dir: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            snapshot_dir: "/etc/netcli/snapshots".into(),
            etc_dir: "/etc".into(),
            pending_file: "/etc/netcli/pending".into(),
            lock_dir: "/tmp/netcli-ctl.lock".into(),
        }
    }
}

/// Commands the CLI can ask for. Numbers stay as the user's text and are
/// validated here, so a huge value is an error message, not a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtlCommand {
    Commit,
    Save,
    CommitConfirmed(String),
    Confirm,
    Rollback(String),
}

pub struct Ctl<'a> {
    pub paths: Paths,
    runner: &'a dyn Runner,
    /// Poll interval of the rollback timer (1 s in production).
    pub tick: Duration,
    /// Seconds since the epoch; only used to label snapshots.
    pub clock: fn() -> u64,
    exe: PathBuf,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn done(text: impl Into<String>) -> Output {
    Output {
        text: format!("{}\n", text.into()),
        ok: true,
    }
}

fn failed(text: impl Into<String>) -> Output {
    Output {
        text: format!("{}\n", text.into()),
        ok: false,
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Snapshot names look like `000012-20260101-120000`.
fn valid_snapshot_name(s: &str) -> bool {
    match s.split_once('-') {
        Some((seq, rest)) => {
            all_digits(seq)
                && !rest.is_empty()
                && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        }
        None => false,
    }
}

fn snapshot_seq(name: &str) -> Option<u64> {
    if !valid_snapshot_name(name) {
        return None;
    }
    name.split_once('-')?.0.parse().ok()
}

/// UCI package file names we are willing to handle.
fn valid_package_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !s.starts_with('-')
}

/// `YYYYMMDD-HHMMSS` (UTC) for a Unix timestamp.
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // days since 1970-01-01 -> civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

/// Regular, visible files of a directory, sorted by name.
fn regular_files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if valid_package_name(&name) {
            v.push((name, e.path()));
        }
    }
    v.sort();
    Ok(v)
}

/// One uci-ctl operation at a time. Released on drop.
struct Lock {
    dir: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

enum Commit {
    Nothing,
    Done(Committed),
    ReloadFailed(Committed, String),
}

struct Committed {
    snap: String,
    changed: Vec<String>,
}

impl<'a> Ctl<'a> {
    pub fn new(paths: Paths, runner: &'a dyn Runner) -> Self {
        Ctl {
            paths,
            runner,
            tick: Duration::from_secs(1),
            clock: now_secs,
            exe: std::env::current_exe().unwrap_or_else(|_| "/usr/bin/netcli".into()),
        }
    }

    pub fn run(&self, cmd: &CtlCommand) -> Output {
        let r = match cmd {
            CtlCommand::Commit => self.cmd_commit(),
            CtlCommand::Save => self.cmd_save(),
            CtlCommand::CommitConfirmed(t) => self.cmd_commit_confirmed(t),
            CtlCommand::Confirm => self.cmd_confirm(),
            CtlCommand::Rollback(n) => self.cmd_rollback(n),
        };
        r.unwrap_or_else(failed)
    }

    fn log(&self, msg: &str) {
        let argv: Vec<String> = ["logger", "-t", "netcli", msg]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let _ = self.runner.run(&argv);
    }

    fn uci(&self, args: &[&str]) -> super::exec::RunResult {
        let mut argv = vec!["uci".to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        self.runner.run(&argv)
    }

    // ------------------------------------------------------------- locking --

    fn acquire_lock(&self) -> R<Lock> {
        let dir = &self.paths.lock_dir;
        let mut tries = 0;
        loop {
            match fs::create_dir(dir) {
                Ok(()) => {
                    let _ = fs::write(dir.join("pid"), std::process::id().to_string());
                    return Ok(Lock { dir: dir.clone() });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let holder = fs::read_to_string(dir.join("pid"))
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    if let Some(pid) = holder {
                        if !pid_alive(pid) {
                            // owner died without cleaning up
                            let _ = fs::remove_dir_all(dir);
                            continue;
                        }
                    }
                    tries += 1;
                    if tries > LOCK_WAIT_TRIES {
                        return Err("another commit/rollback operation is in progress".into());
                    }
                    std::thread::sleep(LOCK_WAIT_STEP);
                }
                Err(e) => return Err(format!("cannot create lock {}: {e}", dir.display())),
            }
        }
    }

    // ----------------------------------------------------------- snapshots --

    /// Newest first. Ordered by the sequence number in the name, so the order
    /// is commit order even if the clock jumps (routers without an RTC).
    fn list_snapshots(&self) -> Vec<String> {
        let mut v: Vec<(u64, String)> = match fs::read_dir(&self.paths.snapshot_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    snapshot_seq(&name).map(|n| (n, name))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        v.sort_by(|a, b| b.cmp(a));
        v.into_iter().map(|(_, n)| n).collect()
    }

    fn take_snapshot(&self) -> R<String> {
        let dir = &self.paths.snapshot_dir;
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let last = fs::read_to_string(dir.join(".seq"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let top = self
            .list_snapshots()
            .first()
            .and_then(|n| snapshot_seq(n))
            .unwrap_or(0);
        let n = last.max(top) + 1;
        let name = format!("{n:06}-{}", format_utc((self.clock)()));
        let tmp = dir.join(format!(".tmp-{n}"));
        let _ = fs::remove_dir_all(&tmp);
        let copy = || -> io::Result<()> {
            fs::create_dir(&tmp)?;
            for (fname, path) in regular_files(&self.paths.etc_dir.join("config"))? {
                fs::copy(path, tmp.join(fname))?;
            }
            fs::rename(&tmp, dir.join(&name))
        };
        if let Err(e) = copy() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!("cannot take snapshot: {e}"));
        }
        let _ = write_atomic(&dir.join(".seq"), n.to_string().as_bytes());
        // prune: flash space is limited
        for old in self.list_snapshots().into_iter().skip(KEEP_SNAPSHOTS) {
            let _ = fs::remove_dir_all(dir.join(old));
        }
        Ok(name)
    }

    fn restore_snapshot(&self, name: &str) -> R<()> {
        if !valid_snapshot_name(name) {
            return Err(format!("invalid snapshot name: {name}"));
        }
        let src = self.paths.snapshot_dir.join(name);
        if !src.is_dir() {
            return Err(format!("snapshot not found: {name}"));
        }
        let cfg = self.paths.etc_dir.join("config");
        let files = regular_files(&src).map_err(|e| format!("{name}: {e}"))?;
        for (fname, path) in files {
            let dst = cfg.join(&fname);
            let mut tmp = dst.as_os_str().to_owned();
            tmp.push(".netcli-new");
            let tmp = PathBuf::from(tmp);
            fs::copy(&path, &tmp)
                .and_then(|_| fs::rename(&tmp, &dst))
                .map_err(|e| format!("cannot restore {fname}: {e}"))?;
        }
        Ok(())
    }

    // --------------------------------------------------------- uci / reload --

    /// Packages with staged changes. `uci changes` lines look like
    ///   network.lan.ipaddr='10.0.0.1'      (set)
    ///   -network.lan.proto                 (delete)
    fn changed_packages(&self) -> R<Vec<String>> {
        let r = self.uci(&["changes"]);
        if !r.success {
            return Err(format!("uci changes failed: {}", r.stderr.trim()));
        }
        let set: BTreeSet<String> = r
            .stdout
            .lines()
            .map(|l| l.trim_start_matches(['-', '+', '~']))
            .map(|l| l.split(['.', '=']).next().unwrap_or("").to_string())
            .filter(|p| !p.is_empty())
            .collect();
        Ok(set.into_iter().collect())
    }

    fn discard_candidate(&self) -> R<()> {
        for pkg in self.changed_packages()? {
            let r = self.uci(&["revert", &pkg]);
            if !r.success {
                return Err(format!("cannot revert {pkg}: {}", r.stderr.trim()));
            }
        }
        Ok(())
    }

    /// Which init scripts to reload for a UCI package. Extend as the CLI grows.
    fn services_for(pkg: &str) -> Vec<&str> {
        match pkg {
            "dhcp" => vec!["dnsmasq", "odhcpd"],
            other => vec![other],
        }
    }

    /// Output of init scripts is discarded on purpose.
    fn reload_services(&self, pkgs: &[String]) -> R<()> {
        let mut failed = Vec::new();
        for pkg in pkgs {
            for svc in Self::services_for(pkg) {
                if !valid_package_name(svc) {
                    continue;
                }
                let script = self.paths.etc_dir.join("init.d").join(svc);
                let runnable = fs::metadata(&script)
                    .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false);
                if !runnable {
                    continue;
                }
                let r = self
                    .runner
                    .run(&[script.to_string_lossy().into_owned(), "reload".to_string()]);
                if !r.success {
                    failed.push(svc.to_string());
                }
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("reload failed for: {}", failed.join(" ")))
        }
    }

    fn do_commit(&self) -> R<Commit> {
        let changed = self.changed_packages()?;
        if changed.is_empty() {
            return Ok(Commit::Nothing);
        }
        let snap = self.take_snapshot()?;
        let r = self.uci(&["commit"]);
        if !r.success {
            // a failed commit must not leave a snapshot behind: it would make
            // `rollback 1` a no-op instead of undoing the previous real commit
            let _ = fs::remove_dir_all(self.paths.snapshot_dir.join(&snap));
            return Err(format!("uci commit failed: {}", r.stderr.trim()));
        }
        let c = Committed { snap, changed };
        Ok(match self.reload_services(&c.changed) {
            Ok(()) => Commit::Done(c),
            Err(why) => Commit::ReloadFailed(c, why),
        })
    }

    /// Stages every package that differs from the running config. Nothing is
    /// written to /etc/config and nothing is reloaded.
    fn load_into_candidate(&self, name: &str) -> R<()> {
        let src = self.paths.snapshot_dir.join(name);
        let files = regular_files(&src).map_err(|e| format!("{name}: {e}"))?;
        let mut staged: Vec<&str> = Vec::new();
        for (pkg, path) in &files {
            let content = fs::read(path).map_err(|e| format!("{pkg}: {e}"))?;
            let running = self.paths.etc_dir.join("config").join(pkg);
            if fs::read(&running).map(|r| r == content).unwrap_or(false) {
                continue;
            }
            let text = String::from_utf8(content)
                .map_err(|_| format!("{pkg}: snapshot file is not valid UTF-8"))?;
            let argv = vec!["uci".to_string(), "import".to_string(), pkg.clone()];
            let r = self.runner.run_stdin(&argv, &text);
            if !r.success {
                for done_pkg in &staged {
                    let _ = self.uci(&["revert", done_pkg]);
                }
                return Err(format!(
                    "cannot load package {pkg} into the candidate: {}",
                    r.stderr.trim()
                ));
            }
            staged.push(pkg);
        }
        Ok(())
    }

    // ------------------------------------------------- pending (confirmed) --

    fn read_pending(&self) -> Option<(String, Vec<String>)> {
        let text = fs::read_to_string(&self.paths.pending_file).ok()?;
        let mut snap = None;
        let mut pkgs = Vec::new();
        for line in text.lines() {
            if let Some(s) = line.strip_prefix("snap=") {
                snap = Some(s.to_string());
            } else if let Some(p) = line.strip_prefix("pkgs=") {
                pkgs = p.split_whitespace().map(String::from).collect();
            }
        }
        snap.filter(|s| valid_snapshot_name(s)).map(|s| (s, pkgs))
    }

    fn pending_is(&self, snap: &str) -> bool {
        matches!(self.read_pending(), Some((s, _)) if s == snap)
    }

    fn write_pending(&self, c: &Committed) -> io::Result<()> {
        if let Some(parent) = self.paths.pending_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = format!("snap={}\npkgs={}\n", c.snap, c.changed.join(" "));
        write_atomic(&self.paths.pending_file, text.as_bytes())
    }

    /// Undo the pending commit. The marker stays if the restore fails, so the
    /// next boot-recover retries.
    fn auto_rollback(&self) -> R<()> {
        let Some((snap, pkgs)) = self.read_pending() else {
            // unreadable marker: it can never be acted on, and would block
            // every later commit-confirmed
            let _ = fs::remove_file(&self.paths.pending_file);
            return Err("pending marker was unreadable and has been removed".into());
        };
        match self.restore_snapshot(&snap) {
            Ok(()) => {
                let _ = fs::remove_file(&self.paths.pending_file);
                if let Err(e) = self.reload_services(&pkgs) {
                    self.log(&format!("reload after auto rollback failed: {e}"));
                }
                Ok(())
            }
            Err(e) => {
                self.log(&format!("auto rollback failed: cannot restore {snap}: {e}"));
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------ commands --

    /// Persist the committed OpenWrt configuration as a boot-time restore point.
    /// OpenWrt's `uci commit` already writes /etc/config to flash; `save` adds
    /// the explicit VyOS-like saved configuration and startup hook requested by
    /// NetCLI users. Refuse to save while candidate changes remain uncommitted.
    fn cmd_save(&self) -> R<Output> {
        let changes = self.uci(&["changes"]);
        if !changes.success {
            return Err(if changes.stderr.is_empty() {
                "cannot inspect candidate configuration before save".into()
            } else {
                changes.stderr
            });
        }
        if !changes.stdout.trim().is_empty() {
            return Err("uncommitted changes exist; run 'compare', then 'commit' before 'save'".into());
        }

        let config_dir = self.paths.etc_dir.join("config");
        let netcli_dir = self.paths.etc_dir.join("netcli");
        let saved_dir = netcli_dir.join("saved-config");
        fs::create_dir_all(&netcli_dir).map_err(|e| format!("cannot create {}: {e}", netcli_dir.display()))?;
        let tmp_dir = netcli_dir.join(".saved-config.tmp");
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir).map_err(|e| format!("cannot clean {}: {e}", tmp_dir.display()))?;
        }
        fs::create_dir(&tmp_dir).map_err(|e| format!("cannot create {}: {e}", tmp_dir.display()))?;

        let files = regular_files(&config_dir).map_err(|e| format!("cannot read {}: {e}", config_dir.display()))?;
        if files.is_empty() {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err("no committed OpenWrt configuration files found to save".into());
        }
        for (name, source) in files {
            let destination = tmp_dir.join(name);
            fs::copy(&source, &destination)
                .map_err(|e| format!("cannot save {}: {e}", source.display()))?;
            let mut perms = fs::metadata(&destination).map_err(|e| e.to_string())?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&destination, perms).map_err(|e| e.to_string())?;
        }

        let old_dir = netcli_dir.join(".saved-config.old");
        if old_dir.exists() {
            fs::remove_dir_all(&old_dir).map_err(|e| format!("cannot remove {}: {e}", old_dir.display()))?;
        }
        if saved_dir.exists() {
            fs::rename(&saved_dir, &old_dir).map_err(|e| format!("cannot rotate saved config: {e}"))?;
        }
        if let Err(e) = fs::rename(&tmp_dir, &saved_dir) {
            if old_dir.exists() { let _ = fs::rename(&old_dir, &saved_dir); }
            return Err(format!("cannot activate saved config: {e}"));
        }
        if old_dir.exists() { let _ = fs::remove_dir_all(&old_dir); }

        // Keep the boot file self-contained and auditable. It restores the
        // saved /etc/config files before OpenWrt starts the network services.
        let boot_file = netcli_dir.join("config.boot");
        let script = b"#!/bin/sh\n# NetCLI saved configuration restore point. Generated by `save`; do not edit by hand.\nset -eu\n[ -d /etc/netcli/saved-config ] || exit 0\nmkdir -p /etc/config\nfor f in /etc/netcli/saved-config/*; do\n    [ -f \"$f\" ] || continue\n    cp -p \"$f\" \"/etc/config/${f##*/}\"\ndone\nexit 0\n";
        write_atomic(&boot_file, script).map_err(|e| format!("cannot write {}: {e}", boot_file.display()))?;
        let mut perms = fs::metadata(&boot_file).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&boot_file, perms).map_err(|e| e.to_string())?;

        Ok(Output { text: format!("Saved {} configuration files to {}\nBoot restore file: {}\n", regular_files(&saved_dir).map(|v| v.len()).unwrap_or(0), saved_dir.display(), boot_file.display()), ok: true })
    }

    fn cmd_commit(&self) -> R<Output> {
        let _lock = self.acquire_lock()?;
        Ok(match self.do_commit()? {
            Commit::Nothing => done("nothing to commit"),
            Commit::Done(c) => done(format!("committed (pre-commit snapshot: {})", c.snap)),
            Commit::ReloadFailed(c, why) => failed(format!(
                "committed (pre-commit snapshot: {}), but reloading services failed ({why})",
                c.snap
            )),
        })
    }

    fn cmd_commit_confirmed(&self, timeout: &str) -> R<Output> {
        let secs = if all_digits(timeout) {
            timeout.parse::<u64>().ok().filter(|s| *s > 0)
        } else {
            None
        }
        .ok_or("usage: commit-confirmed <seconds>  (a positive whole number)")?;
        let _lock = self.acquire_lock()?;
        if self.paths.pending_file.exists() {
            return Err("a commit-confirmed is already pending; run 'confirm' first".into());
        }
        let (committed, reload_problem) = match self.do_commit()? {
            Commit::Nothing => return Ok(done("nothing to commit")),
            Commit::Done(c) => (c, None),
            // Arm the safety net even if the reload failed: that is exactly
            // when the device is most likely to have lost connectivity.
            Commit::ReloadFailed(c, why) => (c, Some(why)),
        };
        self.write_pending(&committed).map_err(|e| {
            format!(
                "committed (snapshot {}), but the rollback marker could not be written ({e}); \
                 the change is NOT protected",
                committed.snap
            )
        })?;
        let started = self.runner.spawn_detached(&[
            self.exe.to_string_lossy().into_owned(),
            "--ctl-timer".to_string(),
            secs.to_string(),
            committed.snap.clone(),
        ]);
        let mut text = format!(
            "committed (pre-commit snapshot: {}), auto-rollback in {secs}s unless 'confirm' is run\n",
            committed.snap
        );
        let mut ok = true;
        if !started {
            text.push_str(
                "warning: could not start the rollback timer; the change is rolled back at the next boot unless confirmed\n",
            );
            ok = false;
        }
        if let Some(why) = reload_problem {
            text.push_str(&format!("warning: reloading services failed ({why})\n"));
            ok = false;
        }
        Ok(Output { text, ok })
    }

    fn cmd_confirm(&self) -> R<Output> {
        let _lock = self.acquire_lock()?;
        if self.paths.pending_file.exists() {
            fs::remove_file(&self.paths.pending_file)
                .map_err(|e| format!("cannot clear the pending marker: {e}"))?;
            Ok(done("commit confirmed"))
        } else {
            Err("no pending confirmation".into())
        }
    }

    fn cmd_rollback(&self, n: &str) -> R<Output> {
        if !all_digits(n) {
            return Err("usage: rollback <n>  (n = 0 discards the candidate)".into());
        }
        let _lock = self.acquire_lock()?;
        let n: u64 = n.parse().unwrap_or(u64::MAX);
        if n == 0 {
            self.discard_candidate()?;
            return Ok(done("candidate discarded"));
        }
        let snaps = self.list_snapshots();
        let name = usize::try_from(n - 1)
            .ok()
            .and_then(|i| snaps.get(i))
            .ok_or_else(|| format!("no snapshot {n} commit(s) back"))?;
        self.load_into_candidate(name)?;
        Ok(done(format!(
            "loaded snapshot {name} ({n} commit(s) back) into the candidate; review with 'show', apply with 'commit'"
        )))
    }

    /// Init-script hook: roll back a commit-confirmed that was pending when
    /// the device went down.
    pub fn boot_recover(&self) -> Output {
        let r = (|| -> R<Output> {
            let _lock = self.acquire_lock()?;
            if !self.paths.pending_file.exists() {
                return Ok(Output {
                    text: String::new(),
                    ok: true,
                });
            }
            self.log("commit-confirmed still pending at boot -- rolling back");
            self.auto_rollback()?;
            Ok(done("rolled back an unconfirmed commit"))
        })();
        r.unwrap_or_else(failed)
    }

    /// The detached timer: runs in its own session after `commit-confirmed`.
    /// Polls instead of one long sleep, so `confirm` (which just removes the
    /// marker) lets it exit at once and a superseded timer exits on its own.
    pub fn run_timer(&self, secs: u64, snap: &str) {
        if !valid_snapshot_name(snap) {
            return;
        }
        for _ in 0..secs {
            std::thread::sleep(self.tick);
            if !self.pending_is(snap) {
                return;
            }
        }
        let Ok(_lock) = self.acquire_lock() else {
            return;
        };
        if !self.pending_is(snap) {
            return;
        }
        self.log(&format!(
            "commit not confirmed within {secs}s -- auto rollback to {snap}"
        ));
        let _ = self.auto_rollback();
    }
}

/// Entry points used by the binary itself, before normal argument handling.
/// Returns the exit status if `args` named an internal command.
pub fn internal_command(args: &[String], paths: &Paths, runner: &dyn Runner) -> Option<i32> {
    match args.get(1).map(String::as_str) {
        Some("--ctl-timer") => {
            let secs = args
                .get(2)
                .filter(|s| all_digits(s))
                .and_then(|s| s.parse().ok());
            match (secs, args.get(3)) {
                (Some(secs), Some(snap)) => {
                    Ctl::new(paths.clone(), runner).run_timer(secs, snap);
                    Some(0)
                }
                _ => {
                    eprintln!("usage: netcli --ctl-timer <seconds> <snapshot>");
                    Some(2)
                }
            }
        }
        Some("--boot-recover") => {
            let out = Ctl::new(paths.clone(), runner).boot_recover();
            eprint!("{}", out.text);
            Some(if out.ok { 0 } else { 1 })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::exec::{RunResult, Runner};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    // ----------------------------------------------------------- test rig --

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "netcli-ctl-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A stateful fake of the system: uci (candidate = files in `stage`),
    /// init scripts, logger and the detached spawner.
    struct Fake {
        stage: PathBuf,
        etc: PathBuf,
        reloads: Mutex<Vec<String>>,
        syslog: Mutex<Vec<String>>,
        spawned: Mutex<Vec<Vec<String>>>,
        commit_fail: AtomicBool,
        reload_fail: AtomicBool,
        import_fail: AtomicBool,
        spawn_ok: AtomicBool,
    }

    struct Shared(Arc<Fake>);

    impl Runner for Shared {
        fn run(&self, argv: &[String]) -> RunResult {
            let f = &self.0;
            let ok = |out: &str| RunResult {
                stdout: out.to_string(),
                stderr: String::new(),
                success: true,
            };
            let err = |msg: &str| RunResult {
                stdout: String::new(),
                stderr: msg.to_string(),
                success: false,
            };
            let init_d = f.etc.join("init.d");
            match argv[0].as_str() {
                "uci" => match argv[1].as_str() {
                    "changes" => {
                        let mut names: Vec<String> = fs::read_dir(&f.stage)
                            .unwrap()
                            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                            .collect();
                        names.sort();
                        ok(&names
                            .iter()
                            .map(|n| format!("{n}.fake='1'\n"))
                            .collect::<String>())
                    }
                    "commit" => {
                        if f.commit_fail.load(Ordering::SeqCst) {
                            return err("uci: Parse error");
                        }
                        for e in fs::read_dir(&f.stage).unwrap() {
                            let e = e.unwrap();
                            fs::copy(e.path(), f.etc.join("config").join(e.file_name())).unwrap();
                            fs::remove_file(e.path()).unwrap();
                        }
                        ok("")
                    }
                    "revert" => {
                        let _ = fs::remove_file(f.stage.join(&argv[2]));
                        ok("")
                    }
                    other => err(&format!("fake uci: unsupported {other}")),
                },
                "logger" => {
                    f.syslog.lock().unwrap().push(argv[3..].join(" "));
                    ok("")
                }
                p if Path::new(p).starts_with(&init_d) => {
                    let svc = Path::new(p).file_name().unwrap().to_string_lossy();
                    f.reloads.lock().unwrap().push(svc.into_owned());
                    // noisy on stdout on purpose: real init scripts are
                    RunResult {
                        stdout: "Warning: noise\n".into(),
                        stderr: String::new(),
                        success: !f.reload_fail.load(Ordering::SeqCst),
                    }
                }
                other => err(&format!("fake: unexpected program {other}")),
            }
        }

        fn run_stdin(&self, argv: &[String], stdin: &str) -> RunResult {
            if argv[..2] == ["uci".to_string(), "import".to_string()] {
                if self.0.import_fail.load(Ordering::SeqCst) {
                    return RunResult {
                        stdout: String::new(),
                        stderr: "uci: Parse error".into(),
                        success: false,
                    };
                }
                fs::write(self.0.stage.join(&argv[2]), stdin).unwrap();
                return RunResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                };
            }
            panic!("unexpected run_stdin {argv:?}");
        }

        fn spawn_detached(&self, argv: &[String]) -> bool {
            self.0.spawned.lock().unwrap().push(argv.to_vec());
            self.0.spawn_ok.load(Ordering::SeqCst)
        }
    }

    struct Rig {
        _tmp: TempDir,
        paths: Paths,
        fake: Arc<Fake>,
    }

    impl Rig {
        fn new() -> Rig {
            let tmp = TempDir::new();
            let root = tmp.0.clone();
            let etc = root.join("etc");
            for d in ["config", "init.d", "netcli"] {
                fs::create_dir_all(etc.join(d)).unwrap();
            }
            fs::create_dir_all(root.join("stage")).unwrap();
            fs::write(etc.join("config/network"), "v1").unwrap();
            fs::write(etc.join("config/firewall"), "f1").unwrap();
            fs::write(etc.join("config/dhcp"), "d1").unwrap();
            for svc in ["network", "firewall", "dnsmasq", "odhcpd"] {
                let p = etc.join("init.d").join(svc);
                fs::write(&p, "#!/bin/sh\n").unwrap();
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
            // a non-executable init script must be skipped
            fs::write(etc.join("init.d/ignored"), "x").unwrap();
            let paths = Paths {
                snapshot_dir: root.join("snap"),
                etc_dir: etc.clone(),
                pending_file: etc.join("netcli/pending"),
                lock_dir: root.join("lock"),
            };
            let fake = Arc::new(Fake {
                stage: root.join("stage"),
                etc,
                reloads: Mutex::new(Vec::new()),
                syslog: Mutex::new(Vec::new()),
                spawned: Mutex::new(Vec::new()),
                commit_fail: AtomicBool::new(false),
                reload_fail: AtomicBool::new(false),
                import_fail: AtomicBool::new(false),
                spawn_ok: AtomicBool::new(true),
            });
            Rig {
                _tmp: tmp,
                paths,
                fake,
            }
        }

        /// Runs `f` with a Ctl that never sleeps.
        fn ctl<T>(&self, f: impl FnOnce(&Ctl) -> T) -> T {
            let shared = Shared(self.fake.clone());
            let mut c = Ctl::new(self.paths.clone(), &shared);
            c.tick = Duration::ZERO;
            f(&c)
        }

        fn run(&self, cmd: CtlCommand) -> Output {
            self.ctl(|c| c.run(&cmd))
        }
        fn commit(&self) -> Output {
            self.run(CtlCommand::Commit)
        }
        fn rollback(&self, n: &str) -> Output {
            self.run(CtlCommand::Rollback(n.into()))
        }
        fn commit_confirmed(&self, n: &str) -> Output {
            self.run(CtlCommand::CommitConfirmed(n.into()))
        }
        fn confirm(&self) -> Output {
            self.run(CtlCommand::Confirm)
        }

        fn stage(&self, pkg: &str, content: &str) {
            fs::write(self.fake.stage.join(pkg), content).unwrap();
        }
        fn staged(&self, pkg: &str) -> Option<String> {
            fs::read_to_string(self.fake.stage.join(pkg)).ok()
        }
        fn staged_count(&self) -> usize {
            fs::read_dir(&self.fake.stage).unwrap().count()
        }
        fn cfg(&self, pkg: &str) -> String {
            fs::read_to_string(self.paths.etc_dir.join("config").join(pkg)).unwrap()
        }
        fn commit_net(&self, v: &str) {
            self.stage("network", v);
            assert!(self.commit().ok);
        }
        fn nsnap(&self) -> usize {
            self.ctl(|c| c.list_snapshots().len())
        }
        fn nreload(&self, svc: &str) -> usize {
            self.fake
                .reloads
                .lock()
                .unwrap()
                .iter()
                .filter(|s| *s == svc)
                .count()
        }
        fn total_reloads(&self) -> usize {
            self.fake.reloads.lock().unwrap().len()
        }
        fn syslog_has(&self, needle: &str) -> bool {
            self.fake
                .syslog
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains(needle))
        }
        /// The snapshot named in the pending marker.
        fn pending_snap(&self) -> String {
            self.ctl(|c| c.read_pending().expect("pending marker").0)
        }
    }

    // -------------------------------------------------------------- tests --

    #[test]
    fn commit_output_is_one_clean_line_even_if_reload_prints() {
        let r = Rig::new();
        r.stage("network", "v2");
        let out = r.commit();
        assert!(out.ok);
        assert_eq!(out.text.lines().count(), 1, "{:?}", out.text);
        let name = out
            .text
            .strip_prefix("committed (pre-commit snapshot: ")
            .and_then(|s| s.strip_suffix(")\n"))
            .unwrap_or_else(|| panic!("unexpected output {:?}", out.text));
        assert!(valid_snapshot_name(name), "{name}");
        assert_eq!(r.cfg("network"), "v2");
    }

    #[test]
    fn empty_commit_is_a_noop() {
        let r = Rig::new();
        let out = r.commit();
        assert!(out.ok);
        assert!(out.text.contains("nothing to commit"));
        assert_eq!(r.nsnap(), 0);
        assert_eq!(r.total_reloads(), 0);
    }

    #[test]
    fn failed_uci_commit_leaves_no_snapshot() {
        let r = Rig::new();
        r.commit_net("v2");
        assert_eq!(r.nsnap(), 1);
        r.stage("network", "v3");
        r.fake.commit_fail.store(true, Ordering::SeqCst);
        let out = r.commit();
        assert!(!out.ok);
        assert!(out.text.contains("Parse error"));
        assert_eq!(r.nsnap(), 1, "no snapshot may be left by a failed commit");
        assert_eq!(r.cfg("network"), "v2");
    }

    #[test]
    fn snapshot_order_survives_clock_going_backwards() {
        static T: AtomicU64 = AtomicU64::new(1_800_000_000);
        fn backwards() -> u64 {
            T.fetch_sub(1000, Ordering::SeqCst)
        }
        let r = Rig::new();
        let shared = Shared(r.fake.clone());
        let mut c = Ctl::new(r.paths.clone(), &shared);
        c.clock = backwards;
        r.stage("network", "v2");
        assert!(c.run(&CtlCommand::Commit).ok);
        r.stage("network", "v3");
        assert!(c.run(&CtlCommand::Commit).ok);
        assert!(r.rollback("1").ok);
        assert_eq!(
            r.staged("network").unwrap(),
            "v2",
            "state before the LAST commit"
        );
        assert!(r.rollback("2").ok);
        assert_eq!(r.staged("network").unwrap(), "v1");
    }

    #[test]
    fn rollback_loads_candidate_without_applying() {
        let r = Rig::new();
        r.commit_net("v2");
        r.commit_net("v3");
        let before = r.total_reloads();
        let out = r.rollback("1");
        assert!(out.ok, "{}", out.text);
        assert_eq!(
            r.cfg("network"),
            "v3",
            "running config untouched by rollback"
        );
        assert_eq!(r.staged("network").unwrap(), "v2");
        assert_eq!(
            r.total_reloads(),
            before,
            "rollback must not reload services"
        );
        let snaps = r.nsnap();
        assert!(r.commit().ok);
        assert_eq!(r.cfg("network"), "v2");
        assert_eq!(
            r.nsnap(),
            snaps + 1,
            "the commit after a rollback can itself be undone"
        );
    }

    #[test]
    fn rollback_skips_packages_identical_to_the_snapshot() {
        let r = Rig::new();
        r.commit_net("v2");
        assert!(r.rollback("1").ok);
        assert!(r.staged("network").is_some());
        assert!(r.staged("firewall").is_none());
        assert!(r.staged("dhcp").is_none());
    }

    #[test]
    fn rollback_message_names_the_snapshot_and_the_next_step() {
        let r = Rig::new();
        r.commit_net("v2");
        let out = r.rollback("1");
        assert!(out.text.contains("into the candidate"));
        assert!(out.text.contains("apply with 'commit'"));
    }

    #[test]
    fn rollback_zero_discards_the_candidate() {
        let r = Rig::new();
        r.stage("network", "v9");
        r.stage("firewall", "f9");
        let out = r.rollback("0");
        assert!(out.ok);
        assert_eq!(out.text, "candidate discarded\n");
        assert_eq!(r.staged_count(), 0);
    }

    #[test]
    fn rollback_beyond_history_fails() {
        let r = Rig::new();
        r.commit_net("v2");
        let out = r.rollback("5");
        assert!(!out.ok);
        assert!(out.text.contains("no snapshot 5"));
        assert!(!r.rollback("99999999999999999999999").ok);
    }

    #[test]
    fn failed_load_does_not_leave_a_half_staged_candidate() {
        let r = Rig::new();
        r.stage("network", "v2");
        r.stage("firewall", "f2");
        assert!(r.commit().ok);
        r.fake.import_fail.store(true, Ordering::SeqCst);
        let out = r.rollback("1");
        assert!(!out.ok);
        assert!(out.text.contains("cannot load package"));
        assert_eq!(r.staged_count(), 0);
    }

    #[test]
    fn bad_arguments_are_usage_errors() {
        let r = Rig::new();
        for n in ["abc", "", "-1", "+1", "1.5"] {
            assert!(!r.rollback(n).ok, "rollback {n:?}");
        }
        for n in ["0", "abc", "", "-5", "+5", "99999999999999999999999"] {
            assert!(!r.commit_confirmed(n).ok, "commit-confirmed {n:?}");
        }
        assert_eq!(r.fake.spawned.lock().unwrap().len(), 0);
    }

    #[test]
    fn only_changed_packages_are_reloaded() {
        let r = Rig::new();
        r.stage("firewall", "f2");
        assert!(r.commit().ok);
        assert_eq!(r.nreload("firewall"), 1);
        assert_eq!(r.nreload("network"), 0);
    }

    #[test]
    fn dhcp_reloads_dnsmasq_and_odhcpd() {
        let r = Rig::new();
        r.stage("dhcp", "d2");
        assert!(r.commit().ok);
        assert_eq!(r.nreload("dnsmasq"), 1);
        assert_eq!(r.nreload("odhcpd"), 1);
        assert_eq!(r.nreload("network"), 0);
    }

    #[test]
    fn services_without_an_executable_init_script_are_skipped() {
        let r = Rig::new();
        r.stage("ignored", "x"); // init.d/ignored exists but is not executable
        r.stage("nosuchservice", "x");
        let out = r.commit();
        assert!(out.ok, "{}", out.text);
        assert_eq!(r.total_reloads(), 0);
    }

    #[test]
    fn reload_failure_is_reported_but_snapshot_kept() {
        let r = Rig::new();
        r.stage("network", "v2");
        r.fake.reload_fail.store(true, Ordering::SeqCst);
        let out = r.commit();
        assert!(!out.ok);
        assert!(out.text.to_lowercase().contains("reload"), "{}", out.text);
        assert_eq!(r.cfg("network"), "v2", "config was committed");
        assert_eq!(r.nsnap(), 1);
    }

    #[test]
    fn confirm_in_time_keeps_the_new_config() {
        let r = Rig::new();
        r.stage("network", "v2");
        let out = r.commit_confirmed("3");
        assert!(out.ok, "{}", out.text);
        assert!(r.paths.pending_file.exists(), "pending marker must exist");
        let snap = r.pending_snap();
        // the timer was started detached, as this same executable
        let spawned = r.fake.spawned.lock().unwrap().clone();
        assert_eq!(spawned.len(), 1);
        assert_eq!(&spawned[0][1..], ["--ctl-timer", "3", snap.as_str()]);
        assert!(r.confirm().ok);
        assert!(!r.paths.pending_file.exists());
        r.ctl(|c| c.run_timer(3, &snap)); // the timer notices and does nothing
        assert_eq!(r.cfg("network"), "v2");
    }

    #[test]
    fn confirm_without_pending_fails() {
        let r = Rig::new();
        let out = r.confirm();
        assert!(!out.ok);
        assert!(out.text.contains("no pending confirmation"));
    }

    #[test]
    fn unconfirmed_commit_rolls_back_even_with_noisy_reload() {
        let r = Rig::new();
        r.stage("network", "v2");
        assert!(r.commit_confirmed("2").ok);
        assert_eq!(r.cfg("network"), "v2");
        let snap = r.pending_snap();
        r.ctl(|c| c.run_timer(2, &snap));
        assert_eq!(
            r.cfg("network"),
            "v1",
            "auto rollback must restore the old config"
        );
        assert!(!r.paths.pending_file.exists());
        assert_eq!(
            r.nreload("network"),
            2,
            "reloaded by the commit and by the rollback"
        );
        assert!(r.syslog_has("auto rollback"));
    }

    #[test]
    fn a_superseded_timer_does_nothing() {
        let r = Rig::new();
        r.stage("network", "v2");
        assert!(r.commit_confirmed("5").ok);
        let first = r.pending_snap();
        assert!(r.confirm().ok);
        r.stage("network", "v3");
        assert!(r.commit_confirmed("5").ok);
        // the first timer wakes up while the second commit is pending
        r.ctl(|c| c.run_timer(5, &first));
        assert_eq!(r.cfg("network"), "v3");
        assert!(r.paths.pending_file.exists());
    }

    #[test]
    fn second_commit_confirmed_is_refused_and_first_stays_armed() {
        let r = Rig::new();
        r.stage("network", "v2");
        assert!(r.commit_confirmed("3").ok);
        let marker = fs::read_to_string(&r.paths.pending_file).unwrap();
        r.stage("network", "v3");
        let out = r.commit_confirmed("60");
        assert!(!out.ok);
        assert!(out.text.contains("already pending"));
        assert_eq!(fs::read_to_string(&r.paths.pending_file).unwrap(), marker);
        assert_eq!(r.cfg("network"), "v2", "the refused commit applied nothing");
        let snap = r.pending_snap();
        r.ctl(|c| c.run_timer(3, &snap));
        assert_eq!(
            r.cfg("network"),
            "v1",
            "the first timer restores the pre-v2 state"
        );
    }

    #[test]
    fn reboot_during_pending_rolls_back_on_boot() {
        let r = Rig::new();
        r.stage("network", "v2");
        assert!(r.commit_confirmed("300").ok);
        // "reboot": the timer process is gone, the marker on flash survives
        let out = r.ctl(|c| c.boot_recover());
        assert!(out.ok, "{}", out.text);
        assert_eq!(r.cfg("network"), "v1");
        assert!(!r.paths.pending_file.exists());
        assert!(r.syslog_has("boot"));
    }

    #[test]
    fn boot_recover_without_pending_is_a_silent_noop() {
        let r = Rig::new();
        let out = r.ctl(|c| c.boot_recover());
        assert!(out.ok);
        assert!(out.text.is_empty());
        assert_eq!(r.cfg("network"), "v1");
        assert_eq!(r.total_reloads(), 0);
    }

    #[test]
    fn boot_recover_clears_a_garbled_marker() {
        let r = Rig::new();
        fs::write(&r.paths.pending_file, "garbage\n").unwrap();
        let out = r.ctl(|c| c.boot_recover());
        assert!(!out.ok);
        assert!(
            !r.paths.pending_file.exists(),
            "must not block later commits forever"
        );
    }

    #[test]
    fn marker_naming_a_missing_snapshot_is_kept_for_a_retry() {
        let r = Rig::new();
        fs::write(
            &r.paths.pending_file,
            "snap=000009-20260101-000000\npkgs=network\n",
        )
        .unwrap();
        let out = r.ctl(|c| c.boot_recover());
        assert!(!out.ok);
        assert!(r.paths.pending_file.exists());
        assert!(r.syslog_has("auto rollback failed"));
    }

    #[test]
    fn marker_with_a_path_traversal_name_is_ignored() {
        let r = Rig::new();
        fs::write(&r.paths.pending_file, "snap=../../etc\npkgs=network\n").unwrap();
        assert!(r.ctl(|c| c.read_pending()).is_none());
    }

    #[test]
    fn commit_confirmed_with_reload_failure_still_arms_the_timer() {
        let r = Rig::new();
        r.stage("network", "v2");
        r.fake.reload_fail.store(true, Ordering::SeqCst);
        let out = r.commit_confirmed("2");
        assert!(!out.ok);
        assert!(
            r.paths.pending_file.exists(),
            "exactly when the safety net matters"
        );
        assert_eq!(r.fake.spawned.lock().unwrap().len(), 1);
        r.fake.reload_fail.store(false, Ordering::SeqCst);
        let snap = r.pending_snap();
        r.ctl(|c| c.run_timer(2, &snap));
        assert_eq!(r.cfg("network"), "v1");
    }

    #[test]
    fn failure_to_start_the_timer_is_reported() {
        let r = Rig::new();
        r.stage("network", "v2");
        r.fake.spawn_ok.store(false, Ordering::SeqCst);
        let out = r.commit_confirmed("30");
        assert!(!out.ok);
        assert!(out.text.contains("could not start the rollback timer"));
        assert!(
            r.paths.pending_file.exists(),
            "boot-recover still protects the change"
        );
    }

    #[test]
    fn nothing_to_commit_with_commit_confirmed_arms_nothing() {
        let r = Rig::new();
        let out = r.commit_confirmed("30");
        assert!(out.ok);
        assert!(out.text.contains("nothing to commit"));
        assert!(!r.paths.pending_file.exists());
        assert!(r.fake.spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn concurrent_commits_do_not_double_commit() {
        let r = Rig::new();
        r.stage("network", "v2");
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let fake = r.fake.clone();
                let paths = r.paths.clone();
                std::thread::spawn(move || {
                    let shared = Shared(fake);
                    Ctl::new(paths, &shared).run(&CtlCommand::Commit)
                })
            })
            .collect();
        let outs: Vec<Output> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let committed = outs
            .iter()
            .filter(|o| o.text.starts_with("committed ("))
            .count();
        let nothing = outs
            .iter()
            .filter(|o| o.text.starts_with("nothing to commit"))
            .count();
        assert_eq!(
            committed,
            1,
            "exactly one commit must win: {outs:?}",
            outs = outs.iter().map(|o| &o.text).collect::<Vec<_>>()
        );
        assert_eq!(nothing, 1, "the other must see nothing left to commit");
        assert_eq!(r.nsnap(), 1);
        assert_eq!(r.cfg("network"), "v2");
    }

    #[test]
    fn stale_lock_is_recovered() {
        let r = Rig::new();
        fs::create_dir(&r.paths.lock_dir).unwrap();
        fs::write(r.paths.lock_dir.join("pid"), "4194999").unwrap(); // above pid_max
        r.stage("network", "v2");
        assert!(r.commit().ok, "commit must steal a stale lock");
        assert_eq!(r.cfg("network"), "v2");
    }

    #[test]
    fn lock_is_released_after_every_command() {
        let r = Rig::new();
        r.stage("network", "v2");
        r.commit();
        r.confirm(); // fails: nothing pending
        r.rollback("0");
        assert!(!r.paths.lock_dir.exists());
    }

    #[test]
    fn snapshots_are_pruned_to_20_and_the_newest_survive() {
        let r = Rig::new();
        for i in 1..=23 {
            r.commit_net(&format!("v{}", i + 1));
        }
        assert_eq!(r.nsnap(), KEEP_SNAPSHOTS);
        assert!(r.rollback("1").ok);
        assert_eq!(
            r.staged("network").unwrap(),
            "v23",
            "rollback 1 after 23 commits"
        );
        // numbering keeps growing after a prune
        let names = r.ctl(|c| c.list_snapshots());
        assert!(names[0].starts_with("000023-"), "{names:?}");
    }

    #[test]
    fn sequence_never_reuses_a_number_even_without_the_seq_file() {
        let r = Rig::new();
        r.commit_net("v2");
        r.commit_net("v3");
        fs::remove_file(r.paths.snapshot_dir.join(".seq")).unwrap();
        r.commit_net("v4");
        let names = r.ctl(|c| c.list_snapshots());
        assert!(names[0].starts_with("000003-"), "{names:?}");
    }

    #[test]
    fn snapshot_is_a_faithful_copy_of_the_config_directory() {
        let r = Rig::new();
        r.commit_net("v2");
        let name = r.ctl(|c| c.list_snapshots()).remove(0);
        let dir = r.paths.snapshot_dir.join(name);
        assert_eq!(fs::read_to_string(dir.join("network")).unwrap(), "v1");
        assert_eq!(fs::read_to_string(dir.join("firewall")).unwrap(), "f1");
        // no temp leftovers
        let leftovers = fs::read_dir(&r.paths.snapshot_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "19700101-000000");
        assert_eq!(format_utc(1_700_000_000), "20231114-221320");
        assert_eq!(format_utc(951_782_400), "20000229-000000"); // leap day
        assert_eq!(format_utc(1_709_251_199), "20240229-235959");
    }

    #[test]
    fn name_validation() {
        assert!(valid_snapshot_name("000001-20260101-120000"));
        for bad in [
            "",
            "x",
            "1",
            "1-",
            "-1",
            "../x",
            "000001-../x",
            "a-1",
            "1-a",
        ] {
            assert!(!valid_snapshot_name(bad), "{bad:?}");
        }
        assert!(valid_package_name("network"));
        for bad in ["", "-x", ".hidden", "a/b", "a b", "a.b"] {
            assert!(!valid_package_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn save_writes_boot_restore_file_and_private_config_snapshot() {
        let r = Rig::new();
        let out = r.ctl(|c| c.run(&CtlCommand::Save));
        assert!(out.ok, "{}", out.text);
        let saved = r.paths.etc_dir.join("netcli/saved-config/network");
        let boot = r.paths.etc_dir.join("netcli/config.boot");
        assert_eq!(fs::read_to_string(saved).unwrap(), "v1");
        assert!(fs::read_to_string(&boot).unwrap().contains("/etc/netcli/saved-config"));
        assert_eq!(
            fs::metadata(boot).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(r.paths.etc_dir.join("netcli/saved-config/network"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn save_refuses_uncommitted_candidate_changes() {
        let r = Rig::new();
        fs::write(r.fake.stage.join("network"), "candidate").unwrap();
        let out = r.ctl(|c| c.run(&CtlCommand::Save));
        assert!(!out.ok);
        assert!(out.text.contains("uncommitted changes exist"));
        assert!(!r.paths.etc_dir.join("netcli/config.boot").exists());
    }

    #[test]
    fn internal_commands() {
        let r = Rig::new();
        let shared = Shared(r.fake.clone());
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            internal_command(&args(&["netcli"]), &r.paths, &shared),
            None
        );
        assert_eq!(
            internal_command(&args(&["netcli", "--script", "x"]), &r.paths, &shared),
            None
        );
        assert_eq!(
            internal_command(&args(&["netcli", "--boot-recover"]), &r.paths, &shared),
            Some(0)
        );
        assert_eq!(
            internal_command(&args(&["netcli", "--ctl-timer"]), &r.paths, &shared),
            Some(2)
        );
        assert_eq!(
            internal_command(
                &args(&["netcli", "--ctl-timer", "x", "y"]),
                &r.paths,
                &shared
            ),
            Some(2)
        );
        // a timer for a snapshot that is not pending exits quietly
        assert_eq!(
            internal_command(
                &args(&["netcli", "--ctl-timer", "0", "000001-20260101-000000"]),
                &r.paths,
                &shared
            ),
            Some(0)
        );
    }
}
