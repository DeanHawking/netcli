//! Executes a matched command by running the real system tools -- uci,
//! ubus, ping and logread; the commit/rollback commands run in-process (ctl.rs).
//!
//! Two steps, so the interesting one is testable without any process:
//!   * `plan`    maps a `Match` to the program invocation (argv). Pure.
//!   * `execute` hands that argv to a `Runner`. `SystemRunner` spawns the
//!     process; tests inject a recording runner.
//!
//! Unlike klish's XML <ACTION> (a shell string with ${var} substitution),
//! every argument is a separate argv element and no shell is involved, so
//! shell injection is structurally impossible. Option injection (a value that
//! looks like `-f`) is prevented at parse time: `Word` and `UciPath` never
//! start with `-`, and values are only ever passed glued to their key
//! (`uci set path=value`).

use super::ctl::{Ctl, CtlCommand, Paths};
use super::render;
use super::cli::command::Action;
use super::cli::parser::Match;
use std::io::Write;
use std::process::{Command, Stdio};

/// Outcome of running one program.
pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub trait Runner {
    fn run(&self, argv: &[String]) -> RunResult;

    /// Like `run`, feeding `stdin` to the program.
    fn run_stdin(&self, argv: &[String], _stdin: &str) -> RunResult {
        RunResult {
            stdout: String::new(),
            stderr: format!("{}: stdin is not supported by this runner\n", argv[0]),
            success: false,
        }
    }

    /// Starts a program fully detached from the calling session (it must
    /// survive a dropped SSH connection). Returns whether it was started.
    fn spawn_detached(&self, _argv: &[String]) -> bool {
        false
    }
}

/// Spawns real processes.
pub struct SystemRunner;

/// PATH for child processes. A login shell started by dropbear may have a
/// PATH without /sbin (where `uci` and `logread` live), so do not rely on it.
/// With the `test-hooks` feature the inherited PATH is kept so tests can put
/// fake programs first.
#[cfg(not(feature = "test-hooks"))]
const CHILD_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

fn command(argv: &[String]) -> Command {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    #[cfg(not(feature = "test-hooks"))]
    c.env("PATH", CHILD_PATH);
    c
}

fn collect(argv: &[String], out: std::io::Result<std::process::Output>) -> RunResult {
    match out {
        Ok(out) => RunResult {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            success: out.status.success(),
        },
        Err(e) => RunResult {
            stdout: String::new(),
            stderr: format!("error running {}: {e}\n", argv[0]),
            success: false,
        },
    }
}

impl Runner for SystemRunner {
    fn run(&self, argv: &[String]) -> RunResult {
        collect(argv, command(argv).output())
    }

    fn run_stdin(&self, argv: &[String], stdin: &str) -> RunResult {
        let mut child = match command(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return collect(argv, Err(e)),
        };
        // write from a thread so a chatty child cannot deadlock us
        let writer = child.stdin.take().map(|mut pipe| {
            let data = stdin.as_bytes().to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })
        });
        let out = child.wait_with_output();
        if let Some(w) = writer {
            let _ = w.join();
        }
        collect(argv, out)
    }

    fn spawn_detached(&self, argv: &[String]) -> bool {
        use std::os::unix::process::CommandExt;
        // setsid gives the child its own session, so the SIGHUP sent to the
        // session when an SSH connection drops cannot reach it. Fall back to
        // nohup, then to a plain new process group.
        for wrapper in [Some("setsid"), Some("nohup"), None] {
            let mut full: Vec<String> = Vec::new();
            full.extend(wrapper.map(String::from));
            full.extend(argv.iter().cloned());
            let mut c = command(&full);
            c.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if wrapper.is_none() {
                c.process_group(0);
            }
            if let Ok(mut child) = c.spawn() {
                // reap it eventually without blocking the caller; if netcli
                // exits first the child is adopted by init and keeps running
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
        }
        false
    }
}

pub struct Env {
    pub ctl_paths: Paths,
    pub runner: Box<dyn Runner>,
}

impl Default for Env {
    fn default() -> Self {
        Env {
            ctl_paths: Paths::default(),
            runner: Box::new(SystemRunner),
        }
    }
}

/// What to do for a matched command.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to run (mode switch, session end).
    Nothing,
    /// Display the operational `show` subcommand list.
    ShowHelp,
    /// Compare staged UCI changes against committed configuration.
    ShowChanges,
    /// Configure an interface IPv4 address, including proto, ipaddr and netmask.
    SetInterfaceAddress { section: &'static str, cidr: String },
    /// Run this program with these arguments.
    Run(Vec<String>),
    /// Commit/rollback logic implemented in-process (`ctl.rs`).
    Ctl(CtlCommand),
    /// Run ubus and lay the JSON out as a table (`render.rs`).
    Show(Show),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Show {
    Interfaces,
    System,
}

fn param<'a>(m: &'a Match, name: &str) -> &'a str {
    m.params
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

fn argv(parts: &[&str]) -> Plan {
    Plan::Run(parts.iter().map(|s| s.to_string()).collect())
}

/// Maps a matched command to the program invocation.
pub fn plan(m: &Match) -> Plan {
    match m.command.action {
        Action::ShowHelp => Plan::ShowHelp,
        Action::ShowInterfaces => Plan::Show(Show::Interfaces),
        Action::ShowInterfacesJson => argv(&["ubus", "call", "network.interface", "dump"]),
        Action::ShowSystem => Plan::Show(Show::System),
        Action::ShowSystemJson => argv(&["ubus", "call", "system", "board"]),
        Action::ShowLog => argv(&["logread"]),
        Action::ShowChanges => Plan::ShowChanges,
        Action::Save => Plan::Ctl(CtlCommand::Save),
        Action::Ping => argv(&["ping", "-c", "3", param(m, "host")]),
        Action::ModeOnly => Plan::Nothing,
        Action::Set => {
            let kv = format!("{}={}", param(m, "path"), param(m, "value"));
            argv(&["uci", "set", &kv])
        }
        Action::SetPath(path) => {
            let kv = format!("{path}={}", param(m, "value"));
            argv(&["uci", "set", &kv])
        }
        Action::SetInterfaceAddress(section) => Plan::SetInterfaceAddress {
            section,
            cidr: param(m, "cidr").to_string(),
        },
        Action::SetConstant(path, value) => {
            let kv = format!("{path}={value}");
            argv(&["uci", "set", &kv])
        }
        Action::AddList(path) => {
            let kv = format!("{path}={}", param(m, "value"));
            argv(&["uci", "add_list", &kv])
        }
        Action::DeletePath(path) => argv(&["uci", "delete", path]),
        Action::Delete => argv(&["uci", "delete", param(m, "path")]),
        Action::Commit => Plan::Ctl(CtlCommand::Commit),
        Action::CommitConfirmed => {
            Plan::Ctl(CtlCommand::CommitConfirmed(param(m, "timeout").to_string()))
        }
        Action::Confirm => Plan::Ctl(CtlCommand::Confirm),
        Action::Rollback => Plan::Ctl(CtlCommand::Rollback(param(m, "n").to_string())),
    }
}

/// Text to show the user, and whether the command succeeded.
pub struct Output {
    pub text: String,
    pub ok: bool,
}

/// Runs the command matched by `m`. Mode switches produce no output here --
/// the caller handles the mode change itself from `m.command.switch_to`.
/// stderr is always shown (warnings matter even when the exit status is 0).
pub fn execute(env: &Env, m: &Match) -> Output {
    match plan(m) {
        Plan::Nothing => Output {
            text: String::new(),
            ok: true,
        },
        Plan::ShowChanges => {
            let r = env.runner.run(&["uci".to_string(), "changes".to_string()]);
            if !r.success {
                Output { text: if r.stderr.is_empty() { r.stdout } else { r.stderr }, ok: false }
            } else if r.stdout.trim().is_empty() {
                Output { text: "No configuration changes.\n".to_string(), ok: true }
            } else {
                let diff = r.stdout.lines().map(|line| {
                    if let Some(rest) = line.strip_prefix('-') {
                        format!("- {rest}\n")
                    } else if let Some(rest) = line.strip_prefix('+') {
                        format!("+ {rest}\n")
                    } else if let Some(rest) = line.strip_prefix('~') {
                        format!("~ {rest}\n")
                    } else {
                        format!("+ {line}\n")
                    }
                }).collect::<String>();
                Output { text: format!("Changes not yet committed:\n{diff}"), ok: true }
            }
        }
        Plan::ShowHelp => Output {
            text: concat!(
                "Available show commands:\n",
                "  show interfaces       Show interface status\n",
                "  show interfaces json  Show interface status as JSON\n",
                "  show system           Show system information\n",
                "  show system json      Show system information as JSON\n",
                "  show log              Show system log\n",
            )
            .to_string(),
            ok: true,
        },
        Plan::Show(show) => show_table(env, show),
        Plan::SetInterfaceAddress { section, cidr } => {
            set_interface_address(env, section, &cidr)
        }
        Plan::Ctl(cmd) => Ctl::new(env.ctl_paths.clone(), env.runner.as_ref()).run(&cmd),
        Plan::Run(argv) => {
            if let Some(section) = network_section_for_write(&argv) {
                if let Err(error) = ensure_network_interface(env, section) {
                    return error;
                }
            }
            let r = env.runner.run(&argv);
            let mut text = r.stdout;
            text.push_str(&r.stderr);
            Output {
                text,
                ok: r.success,
            }
        }
    }
}

/// Return the LAN/WAN section for UCI writes that depend on a named interface.
/// This allows minimal OpenWrt profiles that do not ship a `wan` section to
/// create it on first use instead of failing with `uci: Invalid argument`.
fn network_section_for_write(argv: &[String]) -> Option<&'static str> {
    if argv.len() < 3 || argv.first().map(String::as_str) != Some("uci") {
        return None;
    }
    if !matches!(argv[1].as_str(), "set" | "add_list") {
        return None;
    }
    let path = argv[2].split_once('=').map_or(argv[2].as_str(), |(path, _)| path);
    if path.starts_with("network.lan.") {
        Some("lan")
    } else if path.starts_with("network.wan.") {
        Some("wan")
    } else {
        None
    }
}

fn ensure_network_interface(env: &Env, section: &'static str) -> Result<(), Output> {
    let path = format!("network.{section}");
    let query = ["uci".to_string(), "-q".to_string(), "get".to_string(), path.clone()];
    if env.runner.run(&query).success {
        return Ok(());
    }

    let assignment = format!("{path}=interface");
    let create_args = ["uci".to_string(), "set".to_string(), assignment];
    let create = env.runner.run(&create_args);
    if create.success {
        Ok(())
    } else {
        Err(Output {
            text: format!(
                "{}{}",
                create.stdout,
                if create.stderr.is_empty() {
                    format!("failed to create UCI interface section {path}\n")
                } else {
                    create.stderr
                }
            ),
            ok: false,
        })
    }
}

fn prefix_to_netmask(prefix: u8) -> std::net::Ipv4Addr {
    let bits = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    std::net::Ipv4Addr::from(bits)
}

fn set_interface_address(env: &Env, section: &'static str, cidr: &str) -> Output {
    let Some((address, prefix)) = cidr.split_once('/') else {
        return Output {
            text: format!("IPv4 address must include a prefix length, e.g. {cidr}/24\n"),
            ok: false,
        };
    };
    let Ok(ip) = address.parse::<std::net::Ipv4Addr>() else {
        return Output {
            text: format!("invalid IPv4 address: {address}\n"),
            ok: false,
        };
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return Output {
            text: format!("invalid IPv4 prefix length: {prefix}\n"),
            ok: false,
        };
    };
    if prefix > 32 {
        return Output {
            text: format!("invalid IPv4 prefix length: {prefix}\n"),
            ok: false,
        };
    }
    if let Err(error) = ensure_network_interface(env, section) {
        return error;
    }

    let netmask = prefix_to_netmask(prefix).to_string();
    let values = [
        format!("network.{section}.proto=static"),
        format!("network.{section}.ipaddr={ip}"),
        format!("network.{section}.netmask={netmask}"),
    ];
    for value in values {
        let result = env.runner.run(&[
            "uci".to_string(),
            "set".to_string(),
            value.clone(),
        ]);
        if !result.success {
            let text = if result.stderr.is_empty() {
                format!("failed to set {value}\n")
            } else {
                result.stderr
            };
            return Output {
                text: format!("{}{text}", result.stdout),
                ok: false,
            };
        }
    }
    Output {
        text: format!("staged network.{section} address {ip}/{prefix}\n"),
        ok: true,
    }
}

fn ubus(env: &Env, args: &[&str]) -> RunResult {
    let mut argv = vec!["ubus".to_string(), "call".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    env.runner.run(&argv)
}

/// Runs ubus and renders a table. When ubus fails, its output is shown as
/// is; when the JSON is not what we expect, the raw text is shown instead of
/// nothing (`show ... json` always gives the raw form).
fn show_table(env: &Env, show: Show) -> Output {
    let (primary, rendered) = match show {
        Show::Interfaces => {
            let r = ubus(env, &["network.interface", "dump"]);
            let t = if r.success {
                render::interfaces(&r.stdout)
            } else {
                None
            };
            (r, t)
        }
        Show::System => {
            let r = ubus(env, &["system", "board"]);
            let t = if r.success {
                // uptime and memory are a bonus: failure of this call only drops them
                let info = ubus(env, &["system", "info"]);
                render::system(&r.stdout, info.success.then_some(info.stdout.as_str()))
            } else {
                None
            };
            (r, t)
        }
    };
    let mut text = rendered.unwrap_or(primary.stdout);
    text.push_str(&primary.stderr);
    Output {
        text,
        ok: primary.success,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_core::cli::command::{Mode, ParamType, Token};
    use crate::cli_core::cli::parser::match_line;

    fn plan_of(mode: Mode, words: &[&str]) -> Plan {
        let m = match_line(&crate::commands::REGISTRY, mode, words).expect("line must match");
        plan(&m)
    }

    fn run(parts: &[&str]) -> Plan {
        argv(parts)
    }

    fn argv_vec(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn every_command_in_the_tree_has_a_plan() {
        // Build a valid line for each command and make sure plan() accepts it.
        for mode in [Mode::Operational, Mode::Configuration] {
            for cmd in crate::commands::REGISTRY.commands_for(mode) {
                let words: Vec<&str> = cmd
                    .tokens
                    .iter()
                    .map(|t| match t {
                        Token::Literal(l) => *l,
                        Token::Param { ty, .. } => match ty {
                            ParamType::Uint => "5",
                            ParamType::Hostname => "router1",
                            ParamType::Ipv4 => "192.0.2.1",
                            ParamType::Ipv4Cidr => "192.0.2.1/24",
                            ParamType::Ipv4Netmask => "255.255.255.0",
                            ParamType::DhcpPoolStart => "100",
                            ParamType::DhcpPoolLimit => "150",
                            ParamType::LeaseTime => "12h",
                            ParamType::UciPath => "network.lan.ipaddr",
                            _ => "example.com",
                        },
                    })
                    .collect();
                let m = match_line(&crate::commands::REGISTRY, mode, &words)
                    .unwrap_or_else(|e| panic!("{words:?} must match: {e:?}"));
                // must not panic; ModeOnly is the only way to run nothing
                let p = plan(&m);
                assert_eq!(
                    p == Plan::Nothing,
                    cmd.action == Action::ModeOnly,
                    "{words:?}: only mode-only commands may plan nothing"
                );
            }
        }
    }

    #[test]
    fn show_commands() {
        assert_eq!(plan_of(Mode::Operational, &["show"]), Plan::ShowHelp);
        assert_eq!(
            plan_of(Mode::Operational, &["show", "interfaces"]),
            Plan::Show(Show::Interfaces)
        );
        assert_eq!(
            plan_of(Mode::Operational, &["show", "system"]),
            Plan::Show(Show::System)
        );
        assert_eq!(
            plan_of(Mode::Operational, &["show", "interfaces", "json"]),
            run(&["ubus", "call", "network.interface", "dump"])
        );
        assert_eq!(
            plan_of(Mode::Operational, &["show", "system", "json"]),
            run(&["ubus", "call", "system", "board"])
        );
        assert_eq!(
            plan_of(Mode::Operational, &["show", "log"]),
            run(&["logread"])
        );
        assert_eq!(
            plan_of(Mode::Configuration, &["show"]),
            Plan::ShowChanges
        );
    }

    #[test]
    fn ping_passes_the_host_as_one_argument() {
        assert_eq!(
            plan_of(Mode::Operational, &["ping", "example.com"]),
            run(&["ping", "-c", "3", "example.com"])
        );
    }

    #[test]
    fn set_glues_path_and_value_into_one_argument() {
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "network.lan.ipaddr", "192.168.5.1"]
            ),
            run(&["uci", "set", "network.lan.ipaddr=192.168.5.1"])
        );
    }

    #[test]
    fn set_with_spaces_and_shell_metacharacters_stays_one_argument() {
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "wireless.r.ssid", "a b; reboot $(id) `x`"]
            ),
            run(&["uci", "set", "wireless.r.ssid=a b; reboot $(id) `x`"])
        );
    }

    #[test]
    fn set_with_empty_value() {
        assert_eq!(
            plan_of(Mode::Configuration, &["set", "wireless.r.key", ""]),
            run(&["uci", "set", "wireless.r.key="])
        );
    }

    #[test]
    fn set_accepts_anonymous_section_paths() {
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "firewall.@rule[3].enabled", "0"]
            ),
            run(&["uci", "set", "firewall.@rule[3].enabled=0"])
        );
        assert_eq!(
            plan_of(Mode::Configuration, &["delete", "network.@interface[-1]"]),
            run(&["uci", "delete", "network.@interface[-1]"])
        );
    }

    #[test]
    fn friendly_router_configuration_commands_map_to_uci() {
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "system", "hostname", "router1"]
            ),
            run(&["uci", "set", "system.@system[0].hostname=router1"])
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &[
                    "set",
                    "interfaces",
                    "lan",
                    "ipv4",
                    "address",
                    "192.168.10.1/24"
                ]
            ),
            Plan::SetInterfaceAddress {
                section: "lan",
                cidr: "192.168.10.1/24".to_string(),
            }
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "interfaces", "wan", "protocol", "dhcp"]
            ),
            run(&["uci", "set", "network.wan.proto=dhcp"])
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "interfaces", "wan", "dns", "server", "1.1.1.1"]
            ),
            run(&["uci", "add_list", "network.wan.dns=1.1.1.1"])
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "interfaces", "wan", "peer-dns", "disable"]
            ),
            run(&["uci", "set", "network.wan.peerdns=0"])
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["set", "dhcp", "lan", "server", "disable"]
            ),
            run(&["uci", "set", "dhcp.lan.ignore=1"])
        );
        assert_eq!(
            plan_of(
                Mode::Configuration,
                &["delete", "interfaces", "wan", "dns", "servers"]
            ),
            run(&["uci", "delete", "network.wan.dns"])
        );
    }

    #[test]
    fn compare_and_save_are_registered_as_configuration_commands() {
        assert_eq!(plan_of(Mode::Configuration, &["compare"]), Plan::ShowChanges);
        assert_eq!(plan_of(Mode::Configuration, &["save"]), Plan::Ctl(CtlCommand::Save));
    }

    #[test]
    fn commit_family_is_handled_in_process() {
        assert_eq!(
            plan_of(Mode::Configuration, &["commit"]),
            Plan::Ctl(CtlCommand::Commit)
        );
        assert_eq!(
            plan_of(Mode::Configuration, &["commit-confirmed", "30"]),
            Plan::Ctl(CtlCommand::CommitConfirmed("30".into()))
        );
        assert_eq!(
            plan_of(Mode::Configuration, &["confirm"]),
            Plan::Ctl(CtlCommand::Confirm)
        );
        assert_eq!(
            plan_of(Mode::Configuration, &["rollback", "0"]),
            Plan::Ctl(CtlCommand::Rollback("0".into()))
        );
    }

    #[test]
    fn mode_switches_run_nothing() {
        assert_eq!(plan_of(Mode::Operational, &["configure"]), Plan::Nothing);
        assert_eq!(plan_of(Mode::Operational, &["exit"]), Plan::Nothing);
        assert_eq!(plan_of(Mode::Configuration, &["exit"]), Plan::Nothing);
    }

    #[test]
    fn option_like_parameters_are_rejected_before_planning() {
        use crate::cli_core::cli::parser::MatchError;
        for line in [&["ping", "-f"][..], &["delete", "-q"], &["set", "-t", "x"]] {
            let mode = if line[0] == "ping" {
                Mode::Operational
            } else {
                Mode::Configuration
            };
            assert!(
                matches!(match_line(&crate::commands::REGISTRY, mode, line), Err(MatchError::BadParam { .. })),
                "{line:?} must be rejected"
            );
        }
    }

    #[test]
    fn cidr_prefixes_convert_to_correct_netmasks() {
        assert_eq!(prefix_to_netmask(24).to_string(), "255.255.255.0");
        assert_eq!(prefix_to_netmask(16).to_string(), "255.255.0.0");
        assert_eq!(prefix_to_netmask(32).to_string(), "255.255.255.255");
        assert_eq!(prefix_to_netmask(0).to_string(), "0.0.0.0");
    }

    #[test]
    fn network_address_action_creates_missing_wan_section_and_stages_cidr_fields() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let env = Env {
            ctl_paths: Paths::default(),
            runner: Box::new(RecorderForAddress { calls: calls.clone() }),
        };
        let m = match_line(
            &crate::commands::REGISTRY,
            Mode::Configuration,
            &["set", "interfaces", "wan", "ipv4", "address", "192.168.200.200/24"],
        ).unwrap();
        let result = execute(&env, &m);
        assert!(result.ok, "{}", result.text);
        assert_eq!(
            *calls.borrow(),
            vec![
                argv_vec(&["uci", "-q", "get", "network.wan"]),
                argv_vec(&["uci", "set", "network.wan=interface"]),
                argv_vec(&["uci", "set", "network.wan.proto=static"]),
                argv_vec(&["uci", "set", "network.wan.ipaddr=192.168.200.200"]),
                argv_vec(&["uci", "set", "network.wan.netmask=255.255.255.0"]),
            ]
        );
    }

    struct RecorderForAddress {
        calls: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    }
    impl Runner for RecorderForAddress {
        fn run(&self, argv: &[String]) -> RunResult {
            self.calls.borrow_mut().push(argv.to_vec());
            let missing = argv.first().map(String::as_str) == Some("uci")
                && argv.get(1).map(String::as_str) == Some("-q")
                && argv.get(3).map(String::as_str) == Some("network.wan");
            RunResult {
                stdout: String::new(),
                stderr: String::new(),
                success: !missing,
            }
        }
    }

    /// Answers `ubus call` with canned text; everything else fails.
    struct Ubus {
        dump: Option<&'static str>,
        board: Option<&'static str>,
        info: Option<&'static str>,
    }

    impl Runner for Ubus {
        fn run(&self, argv: &[String]) -> RunResult {
            let key: Vec<&str> = argv.iter().map(String::as_str).collect();
            let answer = match key.as_slice() {
                ["ubus", "call", "network.interface", "dump"] => self.dump,
                ["ubus", "call", "system", "board"] => self.board,
                ["ubus", "call", "system", "info"] => self.info,
                _ => None,
            };
            match answer {
                Some(t) => RunResult {
                    stdout: t.to_string(),
                    stderr: String::new(),
                    success: true,
                },
                None => RunResult {
                    stdout: String::new(),
                    stderr: "Command failed: Not found\n".to_string(),
                    success: false,
                },
            }
        }
    }

    fn show(cmd: &[&str], r: Ubus) -> Output {
        let env = Env {
            ctl_paths: Paths::default(),
            runner: Box::new(r),
        };
        let m = match_line(&crate::commands::REGISTRY, Mode::Operational, cmd).unwrap();
        execute(&env, &m)
    }

    const DUMP: &str = r#"{"interface":[{"interface":"lan","up":true,"uptime":5,"proto":"static","l3_device":"br-lan","ipv4-address":[{"address":"10.0.0.1","mask":24}]}]}"#;

    #[test]
    fn show_interfaces_prints_a_table_not_json() {
        let o = show(
            &["show", "interfaces"],
            Ubus {
                dump: Some(DUMP),
                board: None,
                info: None,
            },
        );
        assert!(o.ok);
        assert!(o.text.starts_with("Interface  Proto"), "{}", o.text);
        assert!(o.text.contains("10.0.0.1/24"));
        assert!(!o.text.contains('{'));
    }

    #[test]
    fn show_interfaces_json_keeps_the_raw_output() {
        let o = show(
            &["show", "interfaces", "json"],
            Ubus {
                dump: Some(DUMP),
                board: None,
                info: None,
            },
        );
        assert_eq!(o.text, DUMP);
    }

    #[test]
    fn show_interfaces_falls_back_to_raw_text_when_the_json_is_unexpected() {
        let o = show(
            &["show", "interfaces"],
            Ubus {
                dump: Some("weird output"),
                board: None,
                info: None,
            },
        );
        assert!(o.ok);
        assert_eq!(o.text, "weird output");
    }

    #[test]
    fn show_interfaces_reports_a_failing_ubus() {
        let o = show(
            &["show", "interfaces"],
            Ubus {
                dump: None,
                board: None,
                info: None,
            },
        );
        assert!(!o.ok);
        assert!(o.text.contains("Command failed"));
    }

    #[test]
    fn show_system_combines_board_and_info() {
        let board = r#"{"hostname":"r1","kernel":"6.12"}"#;
        let info = r#"{"uptime":61}"#;
        let o = show(
            &["show", "system"],
            Ubus {
                dump: None,
                board: Some(board),
                info: Some(info),
            },
        );
        assert!(o.ok);
        assert!(o.text.contains("Hostname  r1"), "{}", o.text);
        assert!(o.text.contains("Uptime    00:01:01"), "{}", o.text);
    }

    #[test]
    fn show_system_survives_a_failing_info_call() {
        let board = r#"{"hostname":"r1"}"#;
        let o = show(
            &["show", "system"],
            Ubus {
                dump: None,
                board: Some(board),
                info: None,
            },
        );
        assert!(o.ok);
        assert!(o.text.contains("Hostname  r1"));
        assert!(!o.text.contains("Uptime"));
    }

    #[test]
    fn system_runner_feeds_stdin() {
        let r = SystemRunner.run_stdin(&["cat".to_string()], "hello\nworld\n");
        assert!(r.success, "{}", r.stderr);
        assert_eq!(r.stdout, "hello\nworld\n");
    }

    #[test]
    fn system_runner_survives_large_stdin() {
        let big = "x".repeat(1 << 20);
        let r = SystemRunner.run_stdin(&["cat".to_string()], &big);
        assert!(r.success);
        assert_eq!(r.stdout.len(), big.len());
    }

    #[test]
    fn detached_processes_are_started_and_keep_running() {
        let file = std::env::temp_dir().join(format!("netcli-detach-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("sleep 0.3; echo done > {}", file.display()),
        ];
        assert!(SystemRunner.spawn_detached(&argv));
        // spawn_detached returned at once; the work finishes on its own
        assert!(!file.exists());
        for _ in 0..100 {
            if file.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(file.exists(), "detached process did not run to completion");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn system_runner_reports_a_missing_program() {
        let r = SystemRunner.run(&["/nonexistent/netcli-test-program".to_string()]);
        assert!(!r.success);
        assert!(r.stderr.contains("error running"));
    }
}
