# NetCLI — Developer Guide

This document explains the architecture of `netcli`, the command execution path, registration of new commands, testing, and contribution rules. It is intended for developers extending the CLI for OpenWrt.

## 1. Purpose and Principles

`netcli` is a console management interface for OpenWrt written in Rust. It operates in an interactive mode, reads commands from a script (`--script`), or reads from standard input if stdin is not a terminal.

Core architectural principles:

- **The core does not know the specific set of commands.** It receives an immutable `CommandRegistry` from the application layer.
- **The command description is the single source of truth.** The parser, `?` help, and `Tab` completions all use the same declarations.
- **Parameters are typed.** A single `ParamType` defines parameter validation during parsing and determines when a command matches an input prefix.
- **Commands execute without shell interpolation.** Programs are launched as `argv` via `std::process::Command`, not as a formatted shell string.
- **Configuration changes are decoupled from applying them.** `set`/`delete` modify candidate UCI; `commit` and `commit-confirmed` go through a controlled mechanism in `core/ctl.rs`.
- **Minimal complexity for the current scale.** The registry uses static slices; parser and completion rely on a deterministic linear search.

## 2. Repository Structure

```text
.
├── .forgejo/workflows/build-all.yml  # CI: formatting, Clippy, tests, binary, and image
├── files/                            # Overlay files for the OpenWrt image
│   └── etc/
│       ├── init.d/netcli-confirm     # Boot restoration of confirmed configuration
│       └── uci-defaults/             # System initialization on first boot
├── netcli-rs/
│   ├── Cargo.toml                    # Dependencies and release profile
│   ├── Cargo.lock                    # Locked dependency versions
│   ├── rust-toolchain.toml            # Rust 1.95.0, rustfmt, Clippy, musl target
│   └── src/
│       ├── main.rs                   # Thin entry point
│       ├── app.rs                    # Assembles application layer and runs CLI
│       ├── commands/                 # Command declarations — domain layer
│       │   ├── mod.rs                # Registry and command group ordering
│       │   ├── operational.rs        # Operational mode commands
│       │   ├── system.rs             # Hostname and system settings
│       │   ├── interfaces.rs         # LAN/WAN, IPv4, gateway, DNS
│       │   ├── dhcp.rs               # DHCP server and LAN pool
│       │   └── configuration.rs      # Generic UCI and transactional commands
│       └── core/                     # CLI core, independent of specific commands
│           ├── mod.rs
│           ├── cli/
│           │   ├── command.rs        # Command, Token, Action, Mode, ParamType types
│           │   ├── parser.rs         # Input matching against command grammar
│           │   └── completion.rs     # Tab completions and ? help from registry
│           ├── tokenize.rs           # String tokenization, handling quotes and ?
│           ├── terminal.rs           # Interactive Reedline/TTY adapter
│           ├── session.rs            # Line processing, mode switching, batch mode
│           ├── exec.rs               # Planning and execution of command actions
│           ├── ctl.rs                # commit, commit-confirmed, confirm, rollback
│           ├── render.rs             # Rendering structured data into tables
│           └── json.rs               # JSON parsing for OpenWrt results
├── packages.txt                      # Packages included in the image
├── run-qemu.sh                       # Launching the built image in QEMU
├── README.md                         # Brief user documentation
└── DEVELOPMENT.md                    # This guide
```

## 3. Roles of Main Layers

### `main.rs` and `app.rs`

`main.rs` only declares modules and calls `app::run()`. `app.rs` is the **composition root**: it builds the `Env`, processes internal service arguments for restoration, and selects one of the execution paths:

1. `netcli --script FILE` — executes the file line by line;
2. If stdin is not a TTY — reads commands from standard input;
3. Otherwise — launches the interactive REPL.

In all modes, the exact same `commands::REGISTRY` is passed. This allows sharing the parser/session logic between REPL and batch mode.

### `core/cli/command.rs` — Command Model

Key types:

- `Mode::Operational` and `Mode::Configuration` — CLI modes;
- `Token::Literal("show")` — fixed keyword;
- `Token::Param { name, ty }` — typed parameter;
- `ParamType` — rules for accepting values (`Hostname`, `Ipv4`, `Ipv4Cidr`, `Ipv4Netmask`, `Uint`, `UciPath`, `LeaseTime`, etc.);
- `Action` — semantic action to execute;
- `Command` — sequence of tokens, help text, action, and optional mode transition;
- `CommandRegistry` — immutable set of command groups for each mode.

`switch_to` specifies mode transition after successful execution. `quits` marks commands that terminate the session entirely.

### `core/cli/parser.rs` — Parsing

`match_line(registry, mode, words)` iterates over the current mode's commands and attempts to match tokens against their declarations. The result is a `Match` containing the command and captured parameters. Errors are classified as unknown command, incomplete command, invalid parameter, or extra arguments.

The current algorithm has a complexity of approximately $O(C \times T)$, where $C$ is the number of commands in the mode and $T$ is the number of tokens in the input. For the current small static registry, this is a simple, deterministic solution without a runtime index. If the registry grows significantly, an index on the first keyword can be added without modifying the `Command` model.

**Registration order matters:** specific user-friendly commands must precede generic patterns like `set <path> <value>` so the parser selects an explicit command rather than a generic fallback.

### `core/cli/completion.rs` — Tab and `?`

Both `suggestions` and `help_entries` read the same `CommandRegistry` as the parser. This prevents scenarios where help suggests a command the parser does not understand. Keywords support unambiguous abbreviations: for instance, `show interfa` resolves to `show interfaces`. If a prefix matches multiple keywords, the parser returns `MatchError::Ambiguous` rather than executing an arbitrary first match.

- `Tab` in `terminal.rs` displays matching literal keywords.
- `?` shows the next keyword or parameter prompt, such as `<value>`, along with the command help text.
- Actual parameter values (IP addresses, hostnames, etc.) are not auto-guessed; they are entered by the administrator.

`terminal.rs` uses `Reedline` with `InputMode::CursorPrefix` and a `ListMenu` restricted to a single line for better serial console behavior. Terminal UI changes must not duplicate command grammar — they must consume `completion.rs`.

### `core/tokenize.rs` — String Tokenization

The tokenizer handles single and double quotes. Quoted values containing spaces become a single token; quotes are removed. Empty strings `""` are supported. Unmatched quotes are treated as part of the actively typed input string. The `help_request` function detects `?` as a help request if the symbol stands as a standalone token at the end of the command.

### `core/session.rs` — Session Management

`process_line` is the common entry point for a single line. It handles `?` requests, tokenizes input, invokes the parser, runs the action, and switches modes if required. A failed command must not switch modes.

`run_batch` reads input line by line and returns an overall success status. The interactive terminal adapter uses the same `process_line`, ensuring command logic does not diverge between scripts and the REPL.

### `core/exec.rs` — Planning and Execution

`plan()` translates an `Action` into an internal `Plan`: executing a program with arguments, rendering a table, running a service transaction operation, or changing mode without invoking an external binary. `execute()` runs the plan and returns output text alongside a success flag.

External processes are executed as an array of arguments (`argv`) rather than through `sh -c`. This is essential: new commands must validate parameters and pass arguments without concatenating shell commands.

`Runner` abstracts external process execution. Production code uses a system runner; unit tests can replace it with a call recorder to verify commands without altering a real system.

### `core/ctl.rs` — Safe Configuration Transactions

`uci set`, `uci delete`, and `uci changes` modify the candidate configuration. `ctl.rs` introduces managed application and recovery:

- `commit` — creates a configuration snapshot, applies UCI commit, and reloads relevant services;
- `commit-confirmed N` — additionally starts an automatic rollback timer;
- `confirm` — cancels a pending automatic rollback;
- `rollback N` — loads an older snapshot into candidate state; `rollback 0` discards uncommitted changes;
- Internal entry points and an init hook ensure recovery of incomplete confirmed operations post-reboot.

Before making network changes that risk dropping SSH connectivity, use `commit-confirmed`, verify access under the new configuration, and only then call `confirm`.

## 4. Adding a Command

### Option A: Command uses an existing action

1. Select a domain file in `src/commands/`: `system.rs`, `interfaces.rs`, `dhcp.rs`, `operational.rs`, or `configuration.rs`.
2. Add a `Command` entry to `COMMANDS`.
3. Define the command in `tokens`; use `Token::Literal` for keywords and `Token::Param` for arguments.
4. Select an existing `ParamType` and `Action`.
5. Add unit tests for parser, completion, and execution planning if needed.

Example declaration (existing LAN IPv4 assignment template):

```rust
Command {
    tokens: &[
        Token::Literal("set"),
        Token::Literal("interfaces"),
        Token::Literal("lan"),
        Token::Literal("ipv4"),
        Token::Literal("address"),
        Token::Param { name: "cidr", ty: ParamType::Ipv4Cidr },
    ],
    help: "Set LAN IPv4 address in CIDR notation",
    action: Action::SetInterfaceAddress("lan"),
    switch_to: None,
    quits: false,
}
```

For `SetPath`, `SetConstant`, `AddList`, `DeletePath`, and other built-in actions, no new core branches are required. The action determines which UCI parameter is written to and how. Logical interface IPv4 addresses use `Action::SetInterfaceAddress`: the address comes in `IPv4/CIDR` format, and the executor sets `proto=static`, `ipaddr`, and the calculated `netmask`. If the `network.lan` or `network.wan` section is missing, the executor creates it as a UCI `interface`; physical `device` properties are specified separately by the admin.

### Option B: Adding a new command domain

1. Create `src/commands/<domain>.rs` and declare `pub const COMMANDS: &[Command]`.
2. Add `pub mod <domain>;` inside `src/commands/mod.rs`.
3. Add `domain::COMMANDS` to the required group in `OPERATIONAL_GROUPS` or `CONFIGURATION_GROUPS`.
4. Verify command signatures do not conflict and ensure specific patterns precede generic ones.

### Option C: Adding new behavior

If no existing `Action` fulfills the requirement:

1. Add a new variant to `Action` in `core/cli/command.rs`.
2. Add translation of this variant to a `Plan` in `core/exec.rs` (`plan`).
3. Add execution logic for the new `Plan` in `execute()`, or build a small dedicated module if the behavior is complex.
4. Add unit tests covering expected `argv` plan mapping as well as session success/failure scenarios.
5. For configuration changes, do not bypass `ctl.rs` if the action should be snapshot/rollback-safe.

### Adding a New Parameter Type

If existing `ParamType` options are insufficient, add a variant and validation logic to `ParamType::matches()` in `core/cli/command.rs`. Then add positive/negative unit tests in `core/cli/parser.rs` (and completion tests if appropriate). Do not implement separate validation logic for parser and Tab: both must utilize the exact same type.

## 5. Current Commands and Modes

The current command set is detailed in `src/commands/` and organized by mode:

- `netcli>` — operational mode: `show`, `show interfaces`, `show system`, `show log`, `ping <host>`, `configure`, `exit`, `logout`;
- `netcli#` — configuration mode: friendly `set ...`, `delete ...`, generic `set <uci-path> <value>`, `show`, `commit`, `commit-confirmed <timeout>`, `confirm`, `rollback <n>`, `exit`.

In configuration mode, `CONFIGURATION_GROUPS` order is intentional: domain commands (`system`, `interfaces`, `dhcp`) precede generic UCI commands defined in `configuration.rs`.

The standalone `show` command declaration varies by mode: in operational mode, it renders help listing `show` subcommands, whereas in configuration mode it displays candidate changes. Long-form read-only subcommands (`show interfaces`, `show system`, `show log`) are registered across both modes.

## 6. Testing and Local Development

Requires a Rust toolchain with `rustfmt`, `clippy`, and `x86_64-unknown-linux-musl` target. The target version is locked in `netcli-rs/rust-toolchain.toml` (`1.95.0`).

Run commands from the `netcli-rs` directory:

```sh
# If Cargo.lock is not yet synced with locked Reedline:
cargo update --package reedline --precise 0.52.1

# Formatting (enforced automatically in CI):
cargo fmt --all
cargo fmt --all -- --check

# Static analysis and unit tests:
cargo clippy --locked --all-targets --features test-hooks -- -D warnings
cargo test --locked --features test-hooks

# Release binary for OpenWrt x86-64:
cargo build --release --target x86_64-unknown-linux-musl --locked
```

The `.forgejo/workflows/build-all.yml` workflow runs formatting checks, Clippy, Rust unit tests, and shell syntax checks; it then builds the static CLI binary for `x86_64-unknown-linux-musl` alongside the target OpenWrt image. CI formats code within its runner checkout but does not commit changes back to Git automatically.

Upon successful builds, test functionality in QEMU and then on target serial/SSH environments. A manual checklist is provided in `VERIFY.md`.

## 7. Mandatory Change Guidelines

- Do not duplicate command grammar separately across parser, help, and completion logic.
- Do not pass raw shell command strings; use `argv` arrays with validated parameters.
- Do not place specific command declarations after generic fallbacks if the matcher resolves to the first matching template.
- Do not apply network modifications blindly: use `commit-confirmed` for changes carrying access loss risks.
- For every newly added command, include tests covering positive cases, invalid parameter handling, and expected actions.
- When changing grammar, verify execution, `?`, and `Tab` completions simultaneously.
- Do not interpret the absence of `tty`/`stty` in minimal OpenWrt builds as proof of TTY absence: it merely indicates missing userland utilities in the root filesystem.

## 8. Scope Limits

NetCLI is intentionally designed as a focused management CLI rather than a full interactive OpenWrt shell. The supported command set is explicitly defined by the registry in `src/commands/`. Before introducing a new subsystem, define its grammar, parameter validation, action, test cases, and expected rollback behavior. Full compilation and execution under QEMU must always be verified via CI or manual testing for the specific code revision.

## Candidate configuration and save/restore workflow

NetCLI configuration mode supports `compare` (and `show`) to inspect staged UCI changes before applying them, `commit` to apply them, and `save` to record the committed `/etc/config` files as an explicit boot restore point. The save operation refuses to run while uncommitted UCI changes exist; use `compare`, then `commit`, then `save`.

`save` creates `/etc/netcli/saved-config/` and the executable `/etc/netcli/config.boot`. The `netcli-config` init hook runs before the normal network startup and restores the saved files. OpenWrt's regular `uci commit` already persists `/etc/config`; this additional save point makes the requested explicit boot configuration visible and restorable. Saving a new snapshot replaces the previous saved snapshot.

## 9. Licensing and Third-Party Components

The original source code of NetCLI is licensed under the **MIT License** (with the project architecture and development supervised and directed by the human author, utilizing AI-assisted generation tools). See the root `LICENSE` file for the official license terms.

### Distribution Notes:
1. **Rust Dependencies:** NetCLI consumes third-party crates (such as `reedline` under MIT). Their respective copyright and license notices must be preserved with binary distributions.
2. **OpenWrt Firmware Images:** Prebuilt OpenWrt images and packaging metadata aggregate various upstream software components licensed under their own terms (including GPLv2 for the Linux kernel and core utilities). Distributing the firmware image requires complying with respective upstream licenses (e.g., providing access to source code where mandated by GPL).
3. **No Warranty:** The software is provided "as is", without warranty of any kind. Always test configuration changes and recovery procedures in a safe environment (such as QEMU) before applying them to production network hardware.