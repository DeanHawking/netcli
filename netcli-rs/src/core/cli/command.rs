//! Shared command descriptors and parameter validators.
//!
//! This module contains the data model only; actual command definitions live
//! in `src/commands/`, and matching/completion algorithms live beside it.

//! Type definitions for modes, parameter validation, and command descriptors.
//! Command instances are supplied by the application through `CommandRegistry`.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Operational,
    Configuration,
}

#[derive(Clone, Copy, Debug)]
pub enum ParamType {
    /// `[A-Za-z0-9_.-]+`, never starting with `-` (it must not be mistaken
    /// for an option by the program it is passed to).
    Word,
    /// `[0-9]+`
    Uint,
    /// A single-label router hostname (letters, digits, hyphens; no leading/trailing hyphen).
    Hostname,
    /// A syntactically valid IPv4 address.
    Ipv4,
    /// IPv4 address with an explicit CIDR prefix, e.g. 192.0.2.1/24.
    Ipv4Cidr,
    /// A contiguous IPv4 netmask.
    Ipv4Netmask,
    /// Positive DHCP pool start offset, up to the 32-bit range.
    DhcpPoolStart,
    /// Positive DHCP pool size, up to the 32-bit range.
    DhcpPoolLimit,
    /// DHCP lease duration, e.g. `12h`, `30m`, or `infinite`.
    LeaseTime,
    /// A dotted UCI path `[A-Za-z0-9_.@\[\]-]+`, never starting with `-`.
    /// The dot avoids treating a bare config package as a valid option path.
    /// Covers named sections (`network.lan.ipaddr`) and anonymous ones
    /// (`firewall.@rule[3].enabled`, `network.@interface[-1]`).
    UciPath,
    /// Free-form remainder of the line (no further tokens after it),
    /// used for values such as `set <path> <value>`. The words are joined
    /// with single spaces; quote the value to keep inner spacing. A quoted
    /// empty string (`""`) is a valid, empty value.
    Rest,
}

impl ParamType {
    pub(crate) fn matches(&self, s: &str) -> bool {
        match self {
            ParamType::Word => {
                !s.is_empty()
                    && !s.starts_with('-')
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            }
            ParamType::Uint => !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()),
            ParamType::Hostname => {
                !s.is_empty()
                    && s.len() <= 63
                    && s.as_bytes()[0].is_ascii_alphanumeric()
                    && s.as_bytes()[s.len() - 1].is_ascii_alphanumeric()
                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }
            ParamType::Ipv4 => s.parse::<std::net::Ipv4Addr>().is_ok(),
            ParamType::Ipv4Cidr => {
                let Some((ip, prefix)) = s.split_once('/') else {
                    return false;
                };
                !ip.is_empty()
                    && !prefix.is_empty()
                    && !prefix.contains('/')
                    && ip.parse::<std::net::Ipv4Addr>().is_ok()
                    && prefix.parse::<u8>().is_ok_and(|n| n <= 32)
            }
            ParamType::Ipv4Netmask => s.parse::<std::net::Ipv4Addr>().is_ok_and(|addr| {
                let mask = u32::from(addr);
                let inverse = !mask;
                mask != 0 && (inverse & inverse.wrapping_add(1)) == 0
            }),
            ParamType::DhcpPoolStart => s.parse::<u32>().is_ok_and(|n| n > 0),
            ParamType::DhcpPoolLimit => s.parse::<u32>().is_ok_and(|n| n > 0),
            ParamType::LeaseTime => {
                s == "infinite"
                    || ["s", "m", "h", "d", "w"].into_iter().any(|unit| {
                        s.strip_suffix(unit)
                            .and_then(|amount| amount.parse::<u32>().ok())
                            .is_some_and(|n| n > 0)
                    })
            }
            ParamType::UciPath => {
                !s.is_empty()
                    && s.contains('.')
                    && !s.starts_with('-')
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-@[]".contains(c))
            }
            // The emptiness of a value is decided by try_match: an empty
            // word is only a value if the user quoted it.
            ParamType::Rest => true,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Token {
    Literal(&'static str),
    Param { name: &'static str, ty: ParamType },
}

/// What a command does. `exec::plan` maps every variant to the program
/// invocation; a unit test guarantees that none is forgotten.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Print available operational `show` subcommands.
    ShowHelp,
    ShowInterfaces,
    ShowInterfacesJson,
    ShowSystem,
    ShowSystemJson,
    ShowLog,
    /// `show` in configuration mode: pending (uncommitted) changes.
    ShowChanges,
    Save,
    Ping,
    /// Pure mode switch / session end: nothing is executed.
    ModeOnly,
    Set,
    /// Friendly command mapping to a fixed UCI option, with a user value.
    SetPath(&'static str),
    /// Set a LAN/WAN address using CIDR; executor writes proto, ipaddr, and netmask.
    SetInterfaceAddress(&'static str),
    /// Friendly command mapping to a fixed UCI option and value.
    SetConstant(&'static str, &'static str),
    /// Append a value to a UCI list option.
    AddList(&'static str),
    /// Delete a fixed UCI option.
    DeletePath(&'static str),
    Delete,
    Commit,
    CommitConfirmed,
    Confirm,
    Rollback,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub tokens: &'static [Token],
    pub help: &'static str,
    pub action: Action,
    /// Target mode to switch to after this command runs (view switch).
    pub switch_to: Option<Mode>,
    /// True for commands that end the whole CLI session (operational
    /// "exit"/"logout"), as opposed to switch_to's "return to a
    /// previous view" (configuration "exit").
    pub quits: bool,
}

/// Immutable source of command definitions supplied by the application layer.
/// The core consumes this interface and does not know which domain modules
/// registered the individual commands.
#[derive(Clone, Copy)]
pub struct CommandRegistry {
    operational: &'static [&'static [Command]],
    configuration: &'static [&'static [Command]],
}

impl CommandRegistry {
    pub const fn new(
        operational: &'static [&'static [Command]],
        configuration: &'static [&'static [Command]],
    ) -> Self {
        Self { operational, configuration }
    }

    /// Iterate command descriptors in stable registration order without
    /// building a heap-backed index at startup.
    pub fn commands_for(&self, mode: Mode) -> impl Iterator<Item = &'static Command> + 'static {
        let groups = match mode {
            Mode::Operational => self.operational,
            Mode::Configuration => self.configuration,
        };
        groups.iter().copied().flatten()
    }
}

/// A non-empty token is a valid command abbreviation when it prefixes a literal.
/// The parser separately detects ambiguous abbreviations before selecting a command.
pub fn literal_matches(typed: &str, literal: &str) -> bool {
    !typed.is_empty() && literal.starts_with(typed)
}

#[derive(Debug, PartialEq, Clone)]
pub enum Suggestion {
    Literal(&'static str),
    ParamHint(&'static str),
}
