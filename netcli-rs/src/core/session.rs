//! One CLI session, independent of the terminal: parse a line, run it, print
//! the result. Shared by batch mode (`--script`, piped stdin) and the
//! interactive REPL, and unit-testable with a recording `Runner`.

use super::exec::{self, Env};
use super::tokenize::{help_request, tokenize};
use super::cli::{help_entries, match_line, CommandRegistry, MatchError, Mode};
use std::io::{BufRead, Write};

pub fn prompt_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Operational => "netcli> ",
        Mode::Configuration => "netcli# ",
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Result of processing one line.
#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub flow: Flow,
    /// False if the line was rejected or the command it ran failed. Batch
    /// mode turns this into the process exit status.
    pub ok: bool,
}

fn describe_error(e: &MatchError) -> String {
    match e {
        MatchError::Unknown => "% Unknown command".to_string(),
        MatchError::Incomplete => "% Incomplete command".to_string(),
        MatchError::TrailingInput => "% Too many arguments".to_string(),
        MatchError::BadParam { name: "cidr", given }
            if given.parse::<std::net::Ipv4Addr>().is_ok() =>
        {
            format!("% IPv4 address must include a prefix length, e.g. {given}/24")
        }
        MatchError::BadParam { name: "cidr", given } => {
            format!("% Invalid IPv4 CIDR '{given}'; use ADDRESS/PREFIX with PREFIX 0-32")
        }
        MatchError::BadParam { name, given } => {
            format!("% Invalid value for '{name}': {given}")
        }
        MatchError::Ambiguous { given, candidates } => {
            format!("% Ambiguous command '{given}': {}", candidates.join(", "))
        }
    }
}

/// Parses and executes one line, mutating `mode` on a view switch and
/// writing output/errors to `out`. Write errors (e.g. a closed pipe) are
/// ignored on purpose: they must not abort the session.
pub fn process_line(registry: &CommandRegistry, mode: &mut Mode, env: &Env, out: &mut dyn Write, line: &str) -> Step {
    let line = line.trim();
    let step = |flow, ok| Step { flow, ok };
    if line.is_empty() {
        return step(Flow::Continue, true);
    }
    if let Some(before) = help_request(line) {
        let tokenized = tokenize(before);
        let words: Vec<&str> = tokenized.iter().map(|s| s.as_str()).collect();
        let entries = help_entries(registry, *mode, &words);
        if entries.is_empty() {
            let _ = writeln!(out, "% No completions available");
        } else {
            for (label, desc) in entries {
                let _ = writeln!(out, "  {label:<20} {desc}");
            }
        }
        return step(Flow::Continue, true);
    }
    let tokenized = tokenize(line);
    let words: Vec<&str> = tokenized.iter().map(|s| s.as_str()).collect();
    match match_line(registry, *mode, &words) {
        Ok(m) => {
            if m.command.quits {
                return step(Flow::Quit, true);
            }
            let result = exec::execute(env, &m);
            let _ = write!(out, "{}", result.text);
            if !result.text.is_empty() && !result.text.ends_with('\n') {
                let _ = writeln!(out);
            }
            if result.ok {
                // a failed command must not switch views
                if let Some(next) = m.command.switch_to {
                    *mode = next;
                }
            }
            step(Flow::Continue, result.ok)
        }
        Err(e) => {
            let _ = writeln!(out, "{}", describe_error(&e));
            step(Flow::Continue, false)
        }
    }
}

/// Reads commands line by line, echoing prompt and command, like a
/// transcript. Returns true only if every line succeeded.
pub fn run_batch<R: BufRead>(registry: &CommandRegistry, reader: R, env: &Env, out: &mut dyn Write) -> bool {
    let mut mode = Mode::Operational;
    let mut all_ok = true;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => return false,
        };
        let _ = writeln!(out, "{}{line}", prompt_str(mode));
        let _ = out.flush();
        let step = process_line(registry, &mut mode, env, out, &line);
        all_ok &= step.ok;
        if step.flow == Flow::Quit {
            break;
        }
    }
    all_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ctl::Paths;
    use super::super::exec::{RunResult, Runner};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Calls = Rc<RefCell<Vec<Vec<String>>>>;

    /// Records every invocation; fails the ones whose first arg is in `fail`.
    struct Recorder {
        calls: Calls,
        fail: Vec<&'static str>,
    }

    impl Runner for Recorder {
        fn run(&self, argv: &[String]) -> RunResult {
            self.calls.borrow_mut().push(argv.to_vec());
            let fails = self.fail.iter().any(|f| argv.iter().any(|a| a == f));
            RunResult {
                stdout: String::new(),
                stderr: if fails {
                    "boom\n".to_string()
                } else {
                    String::new()
                },
                success: !fails,
            }
        }
    }

    fn env_with(fail: Vec<&'static str>) -> (Env, Calls) {
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        // commit/rollback run in-process: keep them away from the real /etc
        let sandbox =
            std::env::temp_dir().join(format!("netcli-session-test-{}", std::process::id()));
        std::fs::create_dir_all(&sandbox).unwrap();
        let env = Env {
            ctl_paths: Paths {
                snapshot_dir: sandbox.join("snap"),
                etc_dir: sandbox.join("etc"),
                pending_file: sandbox.join("pending"),
                lock_dir: sandbox.join("lock"),
            },
            runner: Box::new(Recorder {
                calls: calls.clone(),
                fail,
            }),
        };
        (env, calls)
    }

    fn batch(script: &str, env: &Env) -> (bool, String) {
        let mut out = Vec::new();
        let ok = run_batch(&crate::commands::REGISTRY, script.as_bytes(), env, &mut out);
        (ok, String::from_utf8(out).unwrap())
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn full_configuration_workflow_runs_the_expected_programs() {
        let (env, calls) = env_with(vec![]);
        let script = "show system\nconfigure\nset network.lan.ipaddr 192.168.5.1\nshow\ncommit\ncommit-confirmed 30\nrollback 0\nexit\nexit\n";
        let (ok, out) = batch(script, &env);
        assert!(ok, "transcript:\n{out}");
        assert_eq!(
            *calls.borrow(),
            vec![
                argv(&["ubus", "call", "system", "board"]),
                argv(&["ubus", "call", "system", "info"]),
                argv(&["uci", "-q", "get", "network.lan"]), // ensure the section exists
                argv(&["uci", "set", "network.lan.ipaddr=192.168.5.1"]),
                argv(&["uci", "changes"]), // show
                argv(&["uci", "changes"]), // commit: looks for staged packages
                argv(&["uci", "changes"]), // commit-confirmed
                argv(&["uci", "changes"]), // rollback 0 discards what is staged
            ]
        );
        // commit/rollback are handled in-process and report through the same channel
        assert!(out.contains("nothing to commit\n"));
        assert!(out.contains("candidate discarded\n"));
        // prompt follows the mode
        assert!(out.contains("netcli> configure\n"));
        assert!(out.contains("netcli# set network.lan.ipaddr 192.168.5.1\n"));
        assert!(out.contains("netcli# exit\nnetcli> exit\n"));
    }

    #[test]
    fn bare_ipv4_address_reports_required_prefix_instead_of_generic_value_error() {
        let (env, calls) = env_with(vec![]);
        let mut mode = Mode::Configuration;
        let mut out = Vec::new();
        let step = process_line(
            &crate::commands::REGISTRY,
            &mut mode,
            &env,
            &mut out,
            "set interfaces wan ipv4 address 192.168.200.200",
        );
        assert!(!step.ok);
        assert!(String::from_utf8(out).unwrap().contains(
            "IPv4 address must include a prefix length, e.g. 192.168.200.200/24"
        ));
        assert!(calls.borrow().is_empty(), "invalid command must not invoke UCI");
    }

    #[test]
    fn abbreviated_show_interfaces_works_in_configuration_mode() {
        let (env, calls) = env_with(vec![]);
        let mut mode = Mode::Configuration;
        let mut out = Vec::new();
        let step = process_line(
            &crate::commands::REGISTRY,
            &mut mode,
            &env,
            &mut out,
            "show interfa",
        );
        assert!(step.ok, "{}", String::from_utf8_lossy(&out));
        assert_eq!(
            *calls.borrow(),
            vec![argv(&["ubus", "call", "network.interface", "dump"])]
        );
    }

    #[test]
    fn bare_show_lists_operational_subcommands() {
        let (env, calls) = env_with(vec![]);
        let (ok, out) = batch("show\n", &env);
        assert!(ok, "transcript: {out}");
        assert!(out.contains("Available show commands:"));
        assert!(out.contains("show interfaces"));
        assert!(out.contains("show system"));
        assert!(out.contains("show log"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn a_failing_ctl_command_fails_the_batch() {
        let (env, _) = env_with(vec![]);
        let (ok, out) = batch("configure\nconfirm\n", &env);
        assert!(!ok);
        assert!(out.contains("no pending confirmation"));
    }

    #[test]
    fn session_ends_at_operational_exit_and_ignores_the_rest() {
        let (env, calls) = env_with(vec![]);
        let (ok, _) = batch("exit\nshow log\n", &env);
        assert!(ok);
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn config_commands_are_unknown_in_operational_mode() {
        let (env, calls) = env_with(vec![]);
        let (ok, out) = batch("commit\n", &env);
        assert!(!ok);
        assert!(out.contains("% Unknown command"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn parse_errors_make_batch_fail_but_later_lines_still_run() {
        let (env, calls) = env_with(vec![]);
        let (ok, out) = batch("frobnicate\nshow log\n", &env);
        assert!(!ok);
        assert!(out.contains("% Unknown command"));
        assert_eq!(*calls.borrow(), vec![argv(&["logread"])]);
    }

    #[test]
    fn failed_command_shows_stderr_and_makes_batch_fail() {
        let (env, _) = env_with(vec!["logread"]);
        let (ok, out) = batch("show log\n", &env);
        assert!(!ok);
        assert!(out.contains("boom"));
    }

    #[test]
    fn successful_command_with_stderr_output_still_shows_it() {
        struct Warn;
        impl Runner for Warn {
            fn run(&self, _: &[String]) -> RunResult {
                RunResult {
                    stdout: "ok\n".into(),
                    stderr: "warning: x\n".into(),
                    success: true,
                }
            }
        }
        let env = Env {
            ctl_paths: Paths::default(),
            runner: Box::new(Warn),
        };
        let (ok, out) = batch("show log\n", &env);
        assert!(ok);
        assert!(out.contains("ok\nwarning: x\n"));
    }

    #[test]
    fn option_injection_is_rejected_and_nothing_runs() {
        let (env, calls) = env_with(vec![]);
        let (ok, out) = batch("ping -f\nconfigure\ndelete -q\n", &env);
        assert!(!ok);
        assert!(out.contains("% Invalid value for 'host': -f"));
        assert!(out.contains("% Invalid value for 'path': -q"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn password_ending_in_question_mark_is_set_not_treated_as_help() {
        let (env, calls) = env_with(vec![]);
        let (ok, _) = batch("configure\nset wireless.r.key pass?\n", &env);
        assert!(ok);
        assert_eq!(
            *calls.borrow(),
            vec![argv(&["uci", "set", "wireless.r.key=pass?"])]
        );
    }

    #[test]
    fn empty_value_via_quotes_is_set() {
        let (env, calls) = env_with(vec![]);
        let (ok, _) = batch("configure\nset wireless.r.key \"\"\n", &env);
        assert!(ok);
        assert_eq!(
            *calls.borrow(),
            vec![argv(&["uci", "set", "wireless.r.key="])]
        );
    }

    #[test]
    fn anonymous_section_path_is_accepted() {
        let (env, calls) = env_with(vec![]);
        let (ok, _) = batch("configure\nset firewall.@rule[3].enabled 0\n", &env);
        assert!(ok);
        assert_eq!(
            *calls.borrow(),
            vec![argv(&["uci", "set", "firewall.@rule[3].enabled=0"])]
        );
    }

    #[test]
    fn question_mark_lists_the_next_words() {
        let (env, calls) = env_with(vec![]);
        let (ok, out) = batch("show ?\n?\n", &env);
        assert!(ok);
        assert!(out.contains("interfaces"));
        assert!(out.contains("system"));
        assert!(out.contains("configure"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn help_with_nothing_to_complete() {
        let (env, _) = env_with(vec![]);
        let (ok, out) = batch("show log ?\n", &env);
        assert!(ok);
        assert!(out.contains("% No completions available"));
    }

    #[test]
    fn failed_configure_like_command_does_not_switch_mode() {
        // A mode-switch command currently has no external action that can
        // fail, so exercise the same error path with a normal command and a
        // runner that fails everything. The mode must remain unchanged.
        let (env, _) = env_with(vec!["logread"]);
        let mut mode = Mode::Operational;
        let mut out = Vec::new();
        let step = process_line(&crate::commands::REGISTRY, &mut mode, &env, &mut out, "show log");
        assert!(!step.ok);
        assert_eq!(mode, Mode::Operational);
    }

    #[test]
    fn closed_output_pipe_does_not_panic() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let (env, _) = env_with(vec![]);
        let ok = run_batch(&crate::commands::REGISTRY, "show log\nfrobnicate\n".as_bytes(), &env, &mut Closed);
        assert!(!ok);
    }

    /// Dependency-free fuzz: random lines over a nasty alphabet must never
    /// panic, and no program may ever be started with an option-looking
    /// argument in a host/path position.
    #[test]
    fn random_lines_never_panic_and_never_inject_options() {
        let alphabet: Vec<&str> = vec![
            "show",
            "set",
            "delete",
            "ping",
            "commit",
            "rollback",
            "exit",
            "configure",
            "?",
            "-",
            "-f",
            "--",
            "\"",
            "'",
            " ",
            "  ",
            "network.lan.ipaddr",
            "@",
            "[",
            "]",
            "a;b",
            "$(x)",
            "`",
            "\u{1F980}",
            "0",
            "99999999999999999999",
            "",
            "\t",
        ];
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let n = (next() % 8) as usize;
            let line: String = (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(if next() % 2 == 0 { " " } else { "" });
            let (env, calls) = env_with(vec![]);
            let mut mode = if next() % 2 == 0 {
                Mode::Operational
            } else {
                Mode::Configuration
            };
            let mut sink = Vec::new();
            let _ = process_line(&crate::commands::REGISTRY, &mut mode, &env, &mut sink, &line);
            for c in calls.borrow().iter() {
                if c[0] == "ping" {
                    assert!(!c[3].starts_with('-'), "{line:?} -> {c:?}");
                }
                if c[0] == "uci" && (c[1] == "set" || c[1] == "delete") {
                    assert!(!c[2].starts_with('-'), "{line:?} -> {c:?}");
                }
            }
        }
    }
}
