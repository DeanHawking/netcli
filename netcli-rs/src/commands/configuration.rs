//! Generic UCI operations and configuration transaction commands.
//!
//! Add commands here by describing token grammar, help text, and an existing
//! execution action. Core parsing, validation, and completion require no edits
//! for commands that use an existing action.

use crate::cli_core::cli::command::{Action, Command, Mode, ParamType, Token};

pub const COMMANDS: &[Command] = &[
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Param {
                    name: "path",
                    ty: ParamType::UciPath,
                },
                Token::Param {
                    name: "value",
                    ty: ParamType::Rest,
                },
            ],
            help: "Set a candidate config value using a raw UCI path",
            action: Action::Set,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("delete"),
                Token::Param {
                    name: "path",
                    ty: ParamType::UciPath,
                },
            ],
            help: "Delete a candidate config value",
            action: Action::Delete,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("show")],
            help: "Show pending changes (candidate vs running)",
            action: Action::ShowChanges,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("compare")],
            help: "Compare candidate configuration with committed configuration",
            action: Action::ShowChanges,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("save")],
            help: "Save committed configuration for boot-time restoration",
            action: Action::Save,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("commit")],
            help: "Apply candidate config to running config",
            action: Action::Commit,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("commit-confirmed"),
                Token::Param {
                    name: "timeout",
                    ty: ParamType::Uint,
                },
            ],
            help: "Commit with auto-rollback if not confirmed in time",
            action: Action::CommitConfirmed,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("confirm")],
            help: "Confirm a pending commit-confirmed, cancelling auto-rollback",
            action: Action::Confirm,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("rollback"),
                Token::Param {
                    name: "n",
                    ty: ParamType::Uint,
                },
            ],
            help: "Load the config from N commits back into the candidate (0 = discard candidate)",
            action: Action::Rollback,
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[Token::Literal("exit")],
            help: "Return to operational mode",
            action: Action::ModeOnly,
            switch_to: Some(Mode::Operational),
            quits: false,
        },
];
