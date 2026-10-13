//! Help and completion derived from the same registered command descriptors.
//! This prevents CLI grammar and Tab/? suggestions from drifting apart.

use super::command::{CommandRegistry, Mode, Suggestion, Token};
use super::parser::literal_matches_for_command_prefix;

/// Given the tokens already confirmed on the line (not including the
/// one currently being typed), return what could legally come next.
pub fn suggestions(registry: &CommandRegistry, mode: Mode, words: &[&str]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for cmd in registry.commands_for(mode) {
        if words.len() >= cmd.tokens.len() {
            continue;
        }
        let prefix_ok = words.iter().enumerate().all(|(i, w)| match &cmd.tokens[i] {
            Token::Literal(lit) => literal_matches_for_command_prefix(
                registry,
                mode,
                &words[..i],
                w,
                lit,
            ),
            Token::Param { ty, .. } => ty.matches(w),
        });
        if !prefix_ok {
            continue;
        }
        let next = match &cmd.tokens[words.len()] {
            Token::Literal(lit) => Suggestion::Literal(lit),
            Token::Param { name, .. } => Suggestion::ParamHint(name),
        };
        if !out.contains(&next) {
            out.push(next);
        }
    }
    out
}

/// For `?` help: given the tokens already confirmed, list each
/// possible next word together with a description -- either the
/// full command's help text (if that word completes the command) or
/// a generic "..." marker (if more tokens still follow).
pub fn help_entries(registry: &CommandRegistry, mode: Mode, words: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for cmd in registry.commands_for(mode) {
        if words.len() >= cmd.tokens.len() {
            continue;
        }
        let prefix_ok = words.iter().enumerate().all(|(i, w)| match &cmd.tokens[i] {
            Token::Literal(lit) => literal_matches_for_command_prefix(
                registry,
                mode,
                &words[..i],
                w,
                lit,
            ),
            Token::Param { ty, .. } => ty.matches(w),
        });
        if !prefix_ok {
            continue;
        }
        let next_idx = words.len();
        let label = match &cmd.tokens[next_idx] {
            Token::Literal(lit) => lit.to_string(),
            Token::Param { name, .. } => format!("<{name}>"),
        };
        let is_last = next_idx == cmd.tokens.len() - 1;
        let desc = if is_last {
            cmd.help.to_string()
        } else {
            "...".to_string()
        };
        if !out.iter().any(|(l, _)| l == &label) {
            out.push((label, desc));
        }
    }
    out
}

#[cfg(test)]
mod help_tests {
    use super::*;

    #[test]
    fn help_shows_full_text_for_terminal_word() {
        let h = help_entries(&crate::commands::REGISTRY, Mode::Operational, &["show", "interfaces"][..1]);
        let entry = h.iter().find(|(l, _)| l == "interfaces").unwrap();
        assert_eq!(entry.1, "Show interface status");
    }

    #[test]
    fn help_shows_ellipsis_for_nonterminal_word() {
        let h = help_entries(&crate::commands::REGISTRY, Mode::Operational, &[]);
        let entry = h.iter().find(|(l, _)| l == "show").unwrap();
        assert_eq!(entry.1, "...");
    }

    #[test]
    fn help_shows_ipv4_value_after_address_keyword() {
        let h = help_entries(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "interfaces", "lan", "ipv4", "address"],
        );
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].0, "<cidr>");
        assert_eq!(h[0].1, "Set LAN IPv4 address in CIDR notation, e.g. 192.168.1.1/24");
    }

    #[test]
    fn help_shows_param_placeholder() {
        let h = help_entries(&crate::commands::REGISTRY, Mode::Operational, &["ping"]);
        assert_eq!(h, vec![("<host>".to_string(), "Ping a host".to_string())]);
    }
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;

    #[test]
    fn suggests_top_level_literals() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Operational, &[]);
        assert!(s.contains(&Suggestion::Literal("show")));
        assert!(s.contains(&Suggestion::Literal("ping")));
        assert!(s.contains(&Suggestion::Literal("configure")));
    }

    #[test]
    fn suggests_second_word_after_show() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Operational, &["show"]);
        assert!(s.contains(&Suggestion::Literal("interfaces")));
        assert!(s.contains(&Suggestion::Literal("system")));
        assert!(s.contains(&Suggestion::Literal("log")));
    }

    #[test]
    fn suggests_param_hint_after_ping() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Operational, &["ping"]);
        assert_eq!(s, vec![Suggestion::ParamHint("host")]);
    }

    #[test]
    fn no_suggestions_past_end_of_command() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Operational, &["show", "log"]);
        assert!(s.is_empty());
    }

    #[test]
    fn suggestions_continue_after_an_abbreviated_literal() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Configuration, &["show", "interfa"]);
        assert_eq!(s, vec![Suggestion::Literal("json")]);
    }

    #[test]
    fn suggests_json_after_show_interfaces() {
        let s = suggestions(&crate::commands::REGISTRY, Mode::Operational, &["show", "interfaces"]);
        assert_eq!(s, vec![Suggestion::Literal("json")]);
    }

    #[test]
    fn configuration_completion_follows_the_entered_command_path() {
        let after_set = suggestions(&crate::commands::REGISTRY, Mode::Configuration, &["set"]);
        assert!(after_set.contains(&Suggestion::Literal("system")));
        assert!(after_set.contains(&Suggestion::Literal("interfaces")));
        assert!(after_set.contains(&Suggestion::Literal("dhcp")));
        assert!(!after_set.contains(&Suggestion::Literal("delete")));

        let after_interfaces = suggestions(&crate::commands::REGISTRY, Mode::Configuration, &["set", "interfaces"]);
        assert_eq!(
            after_interfaces,
            vec![Suggestion::Literal("lan"), Suggestion::Literal("wan")]
        );

        let after_lan_ipv4 = suggestions(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "interfaces", "lan", "ipv4"],
        );
        assert_eq!(
            after_lan_ipv4,
            vec![
                Suggestion::Literal("address"),
                Suggestion::Literal("netmask")
            ]
        );
    }

    #[test]
    fn suggests_friendly_router_configuration_values() {
        let protocols = suggestions(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "interfaces", "wan", "protocol"],
        );
        assert_eq!(
            protocols,
            vec![Suggestion::Literal("dhcp"), Suggestion::Literal("static")]
        );

        let address = suggestions(&crate::commands::REGISTRY, 
            Mode::Configuration,
            &["set", "interfaces", "lan", "ipv4", "address"],
        );
        assert_eq!(address, vec![Suggestion::ParamHint("cidr")]);
    }
}

