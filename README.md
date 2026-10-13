# OpenWrt NetCLI

**A small, keyboard-driven command-line interface for administering OpenWrt routers.**

OpenWrt NetCLI is an experimental network-appliance project built around a Rust CLI and a minimal OpenWrt x86-64 image. Its goal is to make common router administration tasks easier to inspect, repeat, automate, and recover from—using a local serial console or SSH instead of requiring a web interface.

The project is open to contributors. Help with testing, command design, documentation, compatibility, and recovery testing is welcome.

> **Project status:** Work in progress. Do not rely on this project as the only way to recover a production router. Test configuration changes in QEMU or on spare hardware first, especially changes that can interrupt network or SSH access.

## Goals

- Provide a clear, keyboard-driven CLI for common OpenWrt administration tasks.
- Make configuration changes inspectable before they are applied.
- Provide snapshots, rollback, and time-limited confirmed commits to reduce the risk of losing remote access.
- Keep the installed image small by omitting LuCI and its web-server packages from the configured image package list.
- Keep the CLI maintainable: command grammar, help, completion, parsing, execution, and tests share a structured command registry.
- Build and test the Rust CLI and OpenWrt image through CI.

## Features

### Interactive CLI

- Runs locally on a console or over SSH.
- Interactive line editing and command history through Reedline.
- `?` help and `Tab` completion driven by the command registry.
- Operational and configuration modes.
- Typed argument validation and support for unambiguous command abbreviations.
- Can read commands from a script or standard input, which is useful for repeatable testing and automation.

### Operational commands

Current commands include:

```text
show
show interfaces
show interfaces json
show system
show system json
show log
ping <host>
configure
exit
logout
```

`show interfaces` and `show system` provide human-readable output; the `json` variants expose raw structured output where supported. The exact information available depends on the OpenWrt services and commands present on the device.

### Configuration workflow

Enter configuration mode with `configure`. Changes are staged through UCI and can be reviewed before applying them:

```text
netcli> configure
netcli# set interfaces lan ipv4 address 192.168.10.1/24
netcli# compare
netcli# commit
netcli# save
netcli# exit
```

The example illustrates the workflow; supported command paths and parameters are defined by the command registry and may evolve.

- `set` — stage a configuration value.
- `delete` — stage removal of a configuration value.
- `compare` — inspect candidate changes before committing.
- `show` — in configuration mode, show pending candidate changes.
- `commit` — apply staged configuration changes and reload the relevant services.
- `save` — create an explicit saved configuration snapshot for boot-time restoration. It refuses to save while uncommitted candidate changes remain.
- `commit-confirmed <seconds>` — apply changes with a time-limited automatic rollback.
- `confirm` — confirm a pending confirmed commit and cancel its automatic rollback.
- `rollback <n>` — load a previous snapshot into the candidate configuration; `rollback 0` discards the current candidate changes.

OpenWrt's normal UCI commit already writes configuration to disk. NetCLI's `save` adds a separate, explicit restore point: it records the committed `/etc/config` files and generates `/etc/netcli/config.boot`. The `netcli-config` init hook restores that saved configuration before normal network startup. The confirmed-commit recovery hook handles a pending rollback after reboot.

**Important:** `save` is not a substitute for a backup stored off the router. Keep independent backups when testing or deploying changes.

### Safer remote changes

Use `commit-confirmed` for changes that could interrupt SSH or network connectivity. If the change is not confirmed within the timeout, NetCLI's recovery mechanism attempts to restore the prior snapshot.

This mechanism needs testing on the target OpenWrt release and device. It cannot guarantee recovery from every hardware failure, storage failure, or incompatible image.

### OpenWrt image

The current image workflow is configured for:

- OpenWrt **25.12.5**
- **x86 / x86-64** (`generic` profile)
- A statically linked Rust `netcli` binary built for `x86_64-unknown-linux-musl`
- A package list in `packages.txt`
- Root-filesystem overlay files under `files/`
- A Forgejo Actions workflow that runs checks, builds the CLI, and assembles the image
- `run-qemu.sh` for booting a built image in QEMU

The package list omits LuCI and the associated web-server packages. OpenWrt components such as UCI, ubus, netifd, firewall4, dnsmasq, odhcpd, and Dropbear remain in the configured list.

## Repository layout

```text
.
├── .forgejo/workflows/build-all.yml  # CI: Rust checks, binary and image builds
├── files/                            # OpenWrt root-filesystem overlay
│   └── etc/
│       ├── init.d/                   # boot-time restore and confirmed-commit recovery
│       ├── uci-defaults/             # first-boot service enablement
│       └── netcli/                   # snapshot storage location
├── netcli-rs/
│   ├── Cargo.toml
│   ├── Cargo.lock
│   └── src/
│       ├── app.rs                    # application setup
│       ├── main.rs                   # entry point
│       ├── commands/                 # command declarations by domain
│       └── core/                     # parser, completion, session, execution, UCI control,
│                                     # snapshots, rollback, JSON and terminal integration
├── packages.txt                      # packages included in the image
├── run-qemu.sh                       # run a built x86-64 image in QEMU
└── DEVELOPMENT.md                    # architecture and contributor notes
```

## Try a built image

A CI build artifact should include the compressed disk image, its checksum/manifest, and `run-qemu.sh`.

With QEMU and the required host tools installed, run:

```sh
chmod +x run-qemu.sh
./run-qemu.sh
```

The script starts the image with a serial console and forwards host TCP port `2222` to the guest's SSH port. Once the image has booted and SSH is configured for your test environment:

```sh
ssh root@127.0.0.1 -p 2222
```

Use a disposable test environment and follow the current build artifact's instructions. Do not expose SSH to an untrusted network.

## Build and test the Rust CLI

Install the Rust toolchain specified in `netcli-rs/rust-toolchain.toml`, including `rustfmt`, Clippy, and the `x86_64-unknown-linux-musl` target. Then:

```sh
cd netcli-rs

cargo fmt --all -- --check
cargo clippy --locked --all-targets --features test-hooks -- -D warnings
cargo test --locked --features test-hooks
cargo build --release --target x86_64-unknown-linux-musl --locked
```

The Forgejo workflow also checks shell scripts, builds the static CLI, downloads the configured OpenWrt ImageBuilder, verifies its checksum, and assembles the image.

The ImageBuilder checksum is checked by default. Verification of the OpenWrt checksum signature is enabled when the `OPENWRT_GPG_FPR` repository variable is configured; without it, CI reports a warning and does not verify that signature.

For more details about internals and adding commands, see [`DEVELOPMENT.md`](DEVELOPMENT.md).

## How the code is organized

- `commands/` declares the supported command syntax, help text, argument types, and action identifiers.
- `core/cli/` contains command types, parsing, completion, and help logic.
- `core/session.rs` handles user input, mode changes, and batch/script execution.
- `core/exec.rs` plans and executes operations.
- `core/ctl.rs` handles configuration commits, confirmed commits, snapshots, confirmation, rollback, and saved-configuration behavior.
- `core/render.rs` formats structured status information for the terminal.
- `core/json.rs` parses the JSON output needed by supported OpenWrt commands.
- `core/terminal.rs` integrates the interactive terminal.

The command registry is intended to be the shared source of truth for parsing, help, and completion. External programs are invoked with argument arrays rather than by constructing shell command strings from user input.

## Contributing

Contributions and issue reports are welcome. Useful areas to help with:

1. **Test coverage:** run the unit tests, report failures, and add regression tests for bugs.
2. **QEMU validation:** test first boot, SSH access, interface status, configuration changes, save/restore, confirmed commit, rollback, and reboot recovery.
3. **OpenWrt compatibility:** verify behavior against supported OpenWrt versions and different x86-64 hardware.
4. **Command coverage:** propose and implement useful operational or configuration commands.
5. **Safety:** review edge cases around failed service reloads, interrupted commits, concurrent operations, and storage errors.
6. **Documentation:** improve examples, clarify command syntax, and report unclear behavior.

When opening an issue, include:

- OpenWrt version and target/device (if applicable)
- The exact command sequence
- Expected behavior and actual behavior
- Relevant output or logs, with passwords, public IPs, keys, and other secrets removed
- Whether the problem reproduces in QEMU or on physical hardware

For code changes, please include tests for new behavior and regressions. Avoid making network changes on a router you cannot physically or otherwise recover.

## Security and recovery notes

- NetCLI is a work in progress; review the code and test it before use on important systems.
- Do not publish configuration backups containing passwords, private keys, tokens, or other secrets.
- Prefer `commit-confirmed` for remote changes that may cut off access.
- Test rollback and boot-time restore before depending on them.
- Keep an independent backup of `/etc/config` and a recovery path outside NetCLI.

## License

This project’s original source code is licensed under the **MIT License** — see the [LICENSE](LICENSE) file for details. 
*(Note: The project was structured, architected, and developed under human supervision with the assistance of AI generation tools).*

### Third-Party & Image Distribution Notes
* **Rust Dependencies:** Dependencies like `reedline` are licensed under their respective open-source terms (MIT/Apache-2.0).
* **OpenWrt Firmware Images:** Prebuilt firmware images aggregate upstream OpenWrt packages and the Linux kernel, which are licensed under **GPLv2** and other open-source licenses. Distributing whole images implies adherence to GPL source distribution requirements for those components.