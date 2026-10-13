//! Deterministic command matching against the registered command descriptors.
//!
//! Complexity is O(C × T), where C is the number of registered commands and T
//! the number of tokens in a command. With a small embedded CLI command set, a
//! linear scan avoids runtime index allocation and keeps matching deterministic.

use super::command::{literal_matches, Command, CommandRegistry, Mode, ParamType, Token};

/// A successfully matched command: which Command matched, plus the
/// captured parameter values in order.
#[derive(Debug)]
pub struct Match {
    pub command: &'static Command,
    pub params: Vec<(&'static str, String)>,
}

#[derive(Debug, PartialEq)]
pub enum MatchError {
    Unknown,
    Incomplete,
    BadParam { name: &'static str, given: String },
    TrailingInput,
    Ambiguous { given: String, candidates: Vec<String> },
}

/// Match a tokenized input line against the commands available in `mode`.
pub fn match_line(registry: &CommandRegistry, mode: Mode, words: &[&str]) -> Result<Match, MatchError> {
    if let Some((given, candidates)) = ambiguous_literal(registry, mode, words) {
        return Err(MatchError::Ambiguous { given, candidates });
    }

    let mut best_err = MatchError::Unknown;
    for cmd in registry.commands_for(mode) {
        match try_match(registry, mode, cmd, words) {
            Ok(m) => return Ok(m),
            Err(e) => {
                // Prefer a meaningful partial/argument error over a generic
                // unknown-command message.
                if matches!(best_err, MatchError::Unknown) {
                    best_err = e;
                }
            }
        }
    }
    Err(best_err)
}

/// Return an ambiguity only when a typed prefix could expand to different
/// keywords at the same position. Exact keywords always win over longer
/// keywords with the same prefix (e.g. `commit` vs `commit-confirmed`).
fn ambiguous_literal(
    registry: &CommandRegistry,
    mode: Mode,
    words: &[&str],
) -> Option<(String, Vec<String>)> {
    for index in 0..words.len() {
        let typed = words[index];
        if typed.is_empty() {
            continue;
        }
        let mut options = std::collections::BTreeSet::<&str>::new();
        for command in registry.commands_for(mode) {
            if command.tokens.len() <= index {
                continue;
            }
            let prior_matches = command.tokens[..index]
                .iter()
                .zip(&words[..index])
                .all(|(token, word)| match token {
                    Token::Literal(literal) => literal_matches(word, literal),
                    Token::Param { ty, .. } => ty.matches(word),
                });
            if !prior_matches {
                continue;
            }
            if let Token::Literal(literal) = &command.tokens[index] {
                if literal_matches(typed, literal) {
                    options.insert(*literal);
                }
            }
        }
        if options.contains(typed) {
            continue;
        }
        if options.len() > 1 {
            return Some((
                typed.to_string(),
                options.into_iter().map(str::to_string).collect(),
            ));
        }
    }
    None
}

/// Apply a prefix only if the user has not already typed another exact
/// keyword at the same grammar position. This prevents `commit 30` from
/// being interpreted as an abbreviation of `commit-confirmed 30`.
pub fn literal_matches_for_command_prefix(
    registry: &CommandRegistry,
    mode: Mode,
    prior_words: &[&str],
    typed: &str,
    literal: &str,
) -> bool {
    if !literal_matches(typed, literal) {
        return false;
    }
    if typed == literal {
        return true;
    }

    for command in registry.commands_for(mode) {
        if command.tokens.len() <= prior_words.len() {
            continue;
        }
        let prior_matches = command.tokens[..prior_words.len()]
            .iter()
            .zip(prior_words)
            .all(|(token, word)| match token {
                Token::Literal(expected) => literal_matches(word, expected),
                Token::Param { ty, .. } => ty.matches(word),
            });
        if !prior_matches {
            continue;
        }
        if let Token::Literal(expected) = &command.tokens[prior_words.len()] {
            if *expected == typed {
                return false;
            }
        }
    }
    true
}

fn try_match(
    registry: &CommandRegistry,
    mode: Mode,
    cmd: &'static Command,
    words: &[&str],
) -> Result<Match, MatchError> {
    let mut params = Vec::new();
    let mut wi = 0;
    for (ti, tok) in cmd.tokens.iter().enumerate() {
        match tok {
            Token::Literal(lit) => {
                let Some(w) = words.get(wi) else {
                    return Err(MatchError::Incomplete);
                };
                if !literal_matches_for_command_prefix(registry, mode, &words[..wi], w, lit) {
                    return Err(MatchError::Unknown);
                }
                wi += 1;
            }
            Token::Param { name, ty } => {
                let is_last = ti == cmd.tokens.len() - 1;
                if matches!(ty, ParamType::Rest) && is_last {
                    if wi >= words.len() {
                        return Err(MatchError::Incomplete);
                    }
                    let rest = words[wi..].join(" ");
                    params.push((*name, rest));
                    wi = words.len();
                    break;
                }
                let Some(w) = words.get(wi) else {
                    return Err(MatchError::Incomplete);
                };
                if !ty.matches(w) {
                    return Err(MatchError::BadParam {
                        name,
                        given: w.to_string(),
                    });
                }
                params.push((*name, w.to_string()));
                wi += 1;
            }
        }
    }
    if wi != words.len() {
        return Err(MatchError::TrailingInput);
    }
    Ok(Match {
        command: cmd,
        params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_core::cli::command::Action;

    #[test]
    fn uci_path_accepts_anonymous_sections_and_rejects_junk() {
        for ok in [
            "network.lan.ipaddr",
            "firewall.@rule[3].enabled",
            "network.@interface[-1]",
            "a_b.c-d",
        ] {
            assert!(
                match_line(&crate::commands::REGISTRY, Mode::Configuration, &["delete", ok]).is_ok(),
                "{ok}"
            );
        }
        for bad in ["system", "-q", "--x", "a b", "a;b", "$(x)", "a`b", ""] {
            assert!(
                matches!(
                    match_line(&crate::commands::REGISTRY, Mode::Configuration, &["delete", bad]),
                    Err(MatchError::BadParam { .. })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn word_and_uint_reject_edge_cases() {
        assert!(match_line(&crate::commands::REGISTRY, Mode::Operational, &["ping", "-f"]).is_err());
        assert!(match_line(&crate::commands::REGISTRY, Mode::Configuration, &["rollback", "-1"]).is_err());
        assert!(match_line(&crate::commands::REGISTRY, Mode::Configuration, &["rollback", ""]).is_err());
        assert!(match_line(&crate::commands::REGISTRY, Mode::Configuration, &["commit-confirmed", "abc"]).is_err());
    }

    #[test]
    fn friendly_config_commands_validate_hostnames_ipv4_and_pool_ranges() {
        let accepts = |words: &[&str]| match_line(&crate::commands::REGISTRY, Mode::Configuration, words).is_ok();
        let rejects = |words: &[&str]| match_line(&crate::commands::REGISTRY, Mode::Configuration, words).is_err();

        assert!(accepts(&["set", "system", "hostname", "router-1"]));
        assert!(rejects(&["set", "system", "hostname", "-router"]));
        assert!(accepts(&[
            "set",
            "interfaces",
            "lan",
            "ipv4",
            "address",
            "192.168.1.1/24"
        ]));
        assert!(rejects(&[
            "set",
            "interfaces",
            "lan",
            "ipv4",
            "address",
            "999.1.1.1/24"
        ]));
        assert!(rejects(&[
            "set",
            "interfaces",
            "lan",
            "ipv4",
            "netmask",
            "255.0.255.0"
        ]));
        assert!(accepts(&["set", "dhcp", "lan", "pool", "start", "1"]));
        assert!(rejects(&["set", "dhcp", "lan", "pool", "start", "0"]));
        assert!(accepts(&["set", "dhcp", "lan", "pool", "limit", "254"]));
        assert!(accepts(&["set", "dhcp", "lan", "pool", "limit", "65536"]));
        assert!(rejects(&["set", "dhcp", "lan", "pool", "limit", "0"]));
        assert!(rejects(&[
            "set",
            "dhcp",
            "lan",
            "pool",
            "limit",
            "4294967296"
        ]));
        assert!(accepts(&["set", "dhcp", "lan", "lease-time", "12h"]));
        assert!(accepts(&["set", "dhcp", "lan", "lease-time", "infinite"]));
        assert!(rejects(&["set", "dhcp", "lan", "lease-time", "later"]));
    }

    #[test]
    fn structured_commands_are_preferred_to_generic_uci_set() {
        let m = match_line(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "system", "hostname", "router1"],
        )
        .unwrap();
        assert_eq!(
            m.command.action,
            Action::SetPath("system.@system[0].hostname")
        );
    }

    #[test]
    fn set_accepts_an_empty_quoted_value() {
        let m = match_line(&crate::commands::REGISTRY, Mode::Configuration, &["set", "a.b.c", ""]).unwrap();
        assert_eq!(m.params.last().unwrap().1, "");
    }

    #[test]
    fn matches_literal_command() {
        let m = match_line(&crate::commands::REGISTRY, Mode::Operational, &["show", "interfaces"]).unwrap();
        assert_eq!(m.command.help, "Show interface status");
        assert!(m.params.is_empty());
    }

    #[test]
    fn matches_command_with_param() {
        let m = match_line(&crate::commands::REGISTRY, Mode::Operational, &["ping", "example.com"]).unwrap();
        assert_eq!(m.params, vec![("host", "example.com".to_string())]);
    }

    #[test]
    fn rejects_bad_param_type() {
        let e = match_line(&crate::commands::REGISTRY, Mode::Configuration, &["rollback", "abc"]).unwrap_err();
        assert!(matches!(e, MatchError::BadParam { .. }));
    }

    #[test]
    fn rejects_unknown_command() {
        let e = match_line(&crate::commands::REGISTRY, Mode::Operational, &["frobnicate"]).unwrap_err();
        assert_eq!(e, MatchError::Unknown);
    }

    #[test]
    fn bare_show_is_a_help_command() {
        let m = match_line(&crate::commands::REGISTRY, Mode::Operational, &["show"]).unwrap();
        assert_eq!(m.command.action, Action::ShowHelp);
    }

    #[test]
    fn unambiguous_command_abbreviations_are_accepted_in_both_modes() {
        let operational = match_line(&crate::commands::REGISTRY, Mode::Operational, &["show", "interfa"]).unwrap();
        assert_eq!(operational.command.action, Action::ShowInterfaces);

        let configuration = match_line(&crate::commands::REGISTRY, Mode::Configuration, &["show", "interfa"]).unwrap();
        assert_eq!(configuration.command.action, Action::ShowInterfaces);
    }

    #[test]
    fn ambiguous_abbreviations_are_reported_instead_of_running_first_match() {
        let error = match_line(&crate::commands::REGISTRY, Mode::Configuration, &["co"]).unwrap_err();
        assert!(matches!(error, MatchError::Ambiguous { .. }));
    }

    #[test]
    fn exact_keyword_does_not_alias_to_a_longer_command_when_extra_args_follow() {
        assert_eq!(
            match_line(&crate::commands::REGISTRY, Mode::Configuration, &["commit", "30"]).unwrap_err(),
            MatchError::TrailingInput,
        );
        let matched = match_line(&crate::commands::REGISTRY, Mode::Configuration, &["commit-c", "30"]).unwrap();
        assert_eq!(matched.command.action, Action::CommitConfirmed);
    }

    #[test]
    fn ipv4_interface_address_requires_cidr_prefix() {
        assert!(match_line(
            &crate::commands::REGISTRY,
            Mode::Configuration,
            &["set", "interfaces", "wan", "ipv4", "address", "192.168.200.200/24"],
        ).is_ok());
        assert!(match_line(
            &crate::commands::REGISTRY,
            Mode::Configuration,
            &["set", "interfaces", "wan", "ipv4", "address", "192.168.200.200"],
        ).is_err());
        assert!(match_line(
            &crate::commands::REGISTRY,
            Mode::Configuration,
            &["set", "interfaces", "wan", "ipv4", "address", "192.168.200.200/33"],
        ).is_err());
    }

    #[test]
    fn rejects_incomplete_command() {
        let e = match_line(&crate::commands::REGISTRY, Mode::Operational, &["ping"]).unwrap_err();
        assert_eq!(e, MatchError::Incomplete);
    }

    #[test]
    fn rejects_trailing_input() {
        let e = match_line(&crate::commands::REGISTRY, Mode::Operational, &["show", "interfaces", "extra"]).unwrap_err();
        assert_eq!(e, MatchError::TrailingInput);
    }

    #[test]
    fn set_rest_param_captures_remaining_words() {
        let m = match_line(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "network.lan.ipaddr", "192.168.1.1"],
        )
        .unwrap();
        assert_eq!(
            m.params,
            vec![
                ("path", "network.lan.ipaddr".to_string()),
                ("value", "192.168.1.1".to_string()),
            ]
        );
    }

    #[test]
    fn view_switch_present_on_configure() {
        let m = match_line(&crate::commands::REGISTRY, Mode::Operational, &["configure"]).unwrap();
        assert_eq!(m.command.switch_to, Some(Mode::Configuration));
    }
}