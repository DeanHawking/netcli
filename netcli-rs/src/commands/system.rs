//! System configuration commands.
//!
//! Add commands here by describing token grammar, help text, and an existing
//! execution action. Core parsing, validation, and completion require no edits
//! for commands that use an existing action.

use crate::cli_core::cli::command::{Action, Command, ParamType, Token};

pub const COMMANDS: &[Command] = &[
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("system"),
                Token::Literal("hostname"),
                Token::Param {
                    name: "value",
                    ty: ParamType::Hostname,
                },
            ],
            help: "Set router hostname",
            action: Action::SetPath("system.@system[0].hostname"),
            switch_to: None,
            quits: false,
        },
];
