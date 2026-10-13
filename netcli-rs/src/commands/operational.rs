//! Operational-mode command declarations.
//!
//! Read-only `show` commands are also available from configuration mode; the
//! bare `show` token keeps its mode-specific meaning (help vs candidate diff).

use crate::cli_core::cli::command::{Action, Command, Mode, ParamType, Token};

pub const SHOW_DETAIL_COMMANDS: &[Command] = &[
    Command {
        tokens: &[Token::Literal("show"), Token::Literal("interfaces")],
        help: "Show interface status",
        action: Action::ShowInterfaces,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[
            Token::Literal("show"),
            Token::Literal("interfaces"),
            Token::Literal("json"),
        ],
        help: "Show interface status as raw JSON",
        action: Action::ShowInterfacesJson,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[Token::Literal("show"), Token::Literal("system")],
        help: "Show system information",
        action: Action::ShowSystem,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[
            Token::Literal("show"),
            Token::Literal("system"),
            Token::Literal("json"),
        ],
        help: "Show system information as raw JSON",
        action: Action::ShowSystemJson,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[Token::Literal("show"), Token::Literal("log")],
        help: "Show system log",
        action: Action::ShowLog,
        switch_to: None,
        quits: false,
    },
];

/// Commands available only in operational mode (besides SHOW_DETAIL_COMMANDS).
pub const COMMANDS: &[Command] = &[
    Command {
        tokens: &[Token::Literal("show")],
        help: "List available show subcommands",
        action: Action::ShowHelp,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[
            Token::Literal("ping"),
            Token::Param {
                name: "host",
                ty: ParamType::Word,
            },
        ],
        help: "Ping a host",
        action: Action::Ping,
        switch_to: None,
        quits: false,
    },
    Command {
        tokens: &[Token::Literal("configure")],
        help: "Enter configuration mode",
        action: Action::ModeOnly,
        switch_to: Some(Mode::Configuration),
        quits: false,
    },
    Command {
        tokens: &[Token::Literal("exit")],
        help: "Exit this CLI session",
        action: Action::ModeOnly,
        switch_to: None,
        quits: true,
    },
    Command {
        tokens: &[Token::Literal("logout")],
        help: "Exit this CLI session",
        action: Action::ModeOnly,
        switch_to: None,
        quits: true,
    },
];
