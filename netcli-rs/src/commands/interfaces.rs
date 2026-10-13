//! LAN/WAN, IPv4, gateway, and DNS configuration commands.
//!
//! Add commands here by describing token grammar, help text, and an existing
//! execution action. Core parsing, validation, and completion require no edits
//! for commands that use an existing action.

use crate::cli_core::cli::command::{Action, Command, ParamType, Token};

pub const COMMANDS: &[Command] = &[
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("lan"),
                Token::Literal("device"),
                Token::Param { name: "value", ty: ParamType::Word },
            ],
            help: "Set the LAN logical interface device, e.g. br-lan",
            action: Action::SetPath("network.lan.device"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("device"),
                Token::Param { name: "value", ty: ParamType::Word },
            ],
            help: "Bind the WAN logical interface to a device, e.g. eth1",
            action: Action::SetPath("network.wan.device"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("lan"),
                Token::Literal("protocol"),
                Token::Literal("static"),
            ],
            help: "Use a static IPv4 address on LAN",
            action: Action::SetConstant("network.lan.proto", "static"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("lan"),
                Token::Literal("ipv4"),
                Token::Literal("address"),
                Token::Param {
                    name: "cidr",
                    ty: ParamType::Ipv4Cidr,
                },
            ],
            help: "Set LAN IPv4 address in CIDR notation, e.g. 192.168.1.1/24",
            action: Action::SetInterfaceAddress("lan"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("lan"),
                Token::Literal("ipv4"),
                Token::Literal("netmask"),
                Token::Param {
                    name: "value",
                    ty: ParamType::Ipv4Netmask,
                },
            ],
            help: "Set LAN IPv4 netmask",
            action: Action::SetPath("network.lan.netmask"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("protocol"),
                Token::Literal("dhcp"),
            ],
            help: "Get WAN IPv4 configuration by DHCP",
            action: Action::SetConstant("network.wan.proto", "dhcp"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("protocol"),
                Token::Literal("static"),
            ],
            help: "Use static IPv4 configuration on WAN",
            action: Action::SetConstant("network.wan.proto", "static"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("ipv4"),
                Token::Literal("address"),
                Token::Param {
                    name: "cidr",
                    ty: ParamType::Ipv4Cidr,
                },
            ],
            help: "Set WAN IPv4 address in CIDR notation, e.g. 192.168.200.200/24",
            action: Action::SetInterfaceAddress("wan"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("ipv4"),
                Token::Literal("netmask"),
                Token::Param {
                    name: "value",
                    ty: ParamType::Ipv4Netmask,
                },
            ],
            help: "Set WAN IPv4 netmask",
            action: Action::SetPath("network.wan.netmask"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("ipv4"),
                Token::Literal("gateway"),
                Token::Param {
                    name: "value",
                    ty: ParamType::Ipv4,
                },
            ],
            help: "Set WAN IPv4 default gateway",
            action: Action::SetPath("network.wan.gateway"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("dns"),
                Token::Literal("server"),
                Token::Param {
                    name: "value",
                    ty: ParamType::Ipv4,
                },
            ],
            help: "Append a custom WAN DNS server (IPv4)",
            action: Action::AddList("network.wan.dns"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("peer-dns"),
                Token::Literal("enable"),
            ],
            help: "Allow DNS servers supplied by the WAN provider",
            action: Action::SetConstant("network.wan.peerdns", "1"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("set"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("peer-dns"),
                Token::Literal("disable"),
            ],
            help: "Ignore DNS servers supplied by the WAN provider",
            action: Action::SetConstant("network.wan.peerdns", "0"),
            switch_to: None,
            quits: false,
        },
        Command {
            tokens: &[
                Token::Literal("delete"),
                Token::Literal("interfaces"),
                Token::Literal("wan"),
                Token::Literal("dns"),
                Token::Literal("servers"),
            ],
            help: "Delete manually configured WAN DNS servers",
            action: Action::DeletePath("network.wan.dns"),
            switch_to: None,
            quits: false,
        },
];
