//! LAN DHCP server and pool configuration commands.
//!
//! Add commands here by describing token grammar, help text, and an existing
//! execution action. Core parsing, validation, and completion require no edits
//! for commands that use an existing action.

use crate::cli_core::cli::command::{Action, Command, ParamType, Token};

pub const COMMANDS: &[Command] = &[
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("dhcp"),
                Token::Literal("lan"),
                Token::Literal("server"),
                Token::Literal("enable"),
            ],
            help: "Enable the LAN DHCP server",
            action: Action::SetConstant("dhcp.lan.ignore", "0"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("dhcp"),
                Token::Literal("lan"),
                Token::Literal("server"),
                Token::Literal("disable"),
            ],
            help: "Disable the LAN DHCP server",
            action: Action::SetConstant("dhcp.lan.ignore", "1"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("dhcp"),
                Token::Literal("lan"),
                Token::Literal("pool"),
                Token::Literal("start"),
                Token::Param {
                    name: "value",
                    ty: ParamType::DhcpPoolStart,
                },
            ],
            help: "Set the first DHCP pool address offset (1..254)",
            action: Action::SetPath("dhcp.lan.start"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("dhcp"),
                Token::Literal("lan"),
                Token::Literal("pool"),
                Token::Literal("limit"),
                Token::Param {
                    name: "value",
                    ty: ParamType::DhcpPoolLimit,
                },
            ],
            help: "Set the DHCP pool size (1..253)",
            action: Action::SetPath("dhcp.lan.limit"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("dhcp"),
                Token::Literal("lan"),
                Token::Literal("lease-time"),
                Token::Param {
                    name: "value",
                    ty: ParamType::LeaseTime,
                },
            ],
            help: "Set DHCP lease time, e.g. 12h, 30m, or infinite",
            action: Action::SetPath("dhcp.lan.leasetime"),
            switch_to: None,
            quits: false,
        },
];
