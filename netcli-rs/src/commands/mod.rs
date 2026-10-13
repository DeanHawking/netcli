//! Declarative command registry. Each domain owns its command metadata; the
//! core parses, validates, documents, and completes these descriptors uniformly.

pub mod configuration;
pub mod dhcp;
pub mod interfaces;
pub mod operational;
pub mod system;

use crate::cli_core::cli::command::{Command, CommandRegistry};

static OPERATIONAL_GROUPS: &[&[Command]] = &[operational::SHOW_DETAIL_COMMANDS, operational::COMMANDS];

// Keep specific, user-friendly configuration commands before the generic
// `set <uci-path> <value>` pattern so the parser prefers structured syntax.
static CONFIGURATION_GROUPS: &[&[Command]] = &[
    operational::SHOW_DETAIL_COMMANDS,
    system::COMMANDS,
    interfaces::COMMANDS,
    dhcp::COMMANDS,
    configuration::COMMANDS,
];

/// App-level registry passed into the core at startup. New command modules are
/// added to the grouped slices below; the parser/completion engine is unchanged.
pub static REGISTRY: CommandRegistry =
    CommandRegistry::new(OPERATIONAL_GROUPS, CONFIGURATION_GROUPS);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_core::cli::command::Mode;

    #[test]
    fn command_signatures_are_unique_within_each_mode() {
        for mode in [Mode::Operational, Mode::Configuration] {
            let mut signatures = std::collections::BTreeSet::new();
            for command in REGISTRY.commands_for(mode) {
                let signature = command.tokens.iter().map(|token| match token {
                    crate::cli_core::cli::command::Token::Literal(value) => (*value).to_string(),
                    crate::cli_core::cli::command::Token::Param { name, ty } => format!("<{name}:{ty:?}>"),
                }).collect::<Vec<_>>().join(" ");
                assert!(signatures.insert(signature.clone()), "duplicate command signature: {signature}");
            }
        }
    }

    #[test]
    fn structured_commands_precede_generic_uci_fallback() {
        let commands = REGISTRY.commands_for(Mode::Configuration).collect::<Vec<_>>();
        let hostname = commands.iter().position(|command| command.help == "Set router hostname").unwrap();
        let generic = commands.iter().position(|command| command.help == "Set a candidate config value using a raw UCI path").unwrap();
        assert!(hostname < generic);
    }
}
