//! Process entry points: internal rollback hooks, batch/script mode, and REPL.
use crate::cli_core::{ctl, exec::Env, session::run_batch, terminal::run_repl};
use std::io::{self, IsTerminal};

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let env = Env::default();

    // Internal entry points: detached rollback timer and boot hook.
    if let Some(code) = ctl::internal_command(&args, &env.ctl_paths, env.runner.as_ref()) {
        std::process::exit(code);
    }

    if let Some(i) = args.iter().position(|a| a == "--script") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("netcli: --script needs a file name");
            std::process::exit(2);
        };
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("netcli: cannot open script file {path}: {e}");
                std::process::exit(2);
            }
        };
        let ok = run_batch(&crate::commands::REGISTRY, io::BufReader::new(file), &env, &mut io::stdout());
        std::process::exit(if ok { 0 } else { 1 });
    }

    if !io::stdin().is_terminal() {
        let ok = run_batch(&crate::commands::REGISTRY, io::stdin().lock(), &env, &mut io::stdout());
        std::process::exit(if ok { 0 } else { 1 });
    }

    run_repl(&crate::commands::REGISTRY);
}
