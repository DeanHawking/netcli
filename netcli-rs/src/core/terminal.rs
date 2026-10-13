//! Interactive terminal adapter. Core session/command code stays independent
//! from Reedline, allowing the same engine to serve scripts and a TTY.

use super::exec::Env;
use super::session::{self, process_line, Flow};
use super::tokenize::tokenize;
use crate::cli_core::cli::completion::suggestions;
use crate::cli_core::cli::command::{CommandRegistry, Mode, Suggestion};
use std::io;
use std::sync::{Arc, Mutex};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, Completer as ReedlineCompleter, DefaultPrompt,
    DefaultPromptSegment, Emacs, InputMode, KeyCode, KeyModifiers, MenuBuilder,
    Reedline, ReedlineEvent, ReedlineMenu, Signal, Span,
};

pub fn run_repl(registry: &'static CommandRegistry) {
    struct NetcliCompleter {
        mode: Arc<Mutex<Mode>>,
        registry: &'static CommandRegistry,
    }

    impl ReedlineCompleter for NetcliCompleter {
        fn complete(&mut self, line: &str, pos: usize) -> reedline::CompletionResult {
            let (before, _after) = line.split_at(pos.min(line.len()));
            let ends_with_space = before.ends_with(' ') || before.is_empty();

            let tokenized = tokenize(before);
            let words: Vec<&str> = tokenized.iter().map(|s| s.as_str()).collect();

            let (confirmed, partial): (&[&str], &str) = if ends_with_space {
                (&words[..], "")
            } else {
                (
                    &words[..words.len().saturating_sub(1)],
                    words.last().copied().unwrap_or(""),
                )
            };
            let start = before.len() - partial.len();
            let cur_mode = *self.mode.lock().unwrap();
            let completions = suggestions(self.registry, cur_mode, confirmed)
                .into_iter()
                .filter_map(|s| match s {
                    Suggestion::Literal(lit) if lit.starts_with(partial) => {
                        Some(reedline::Suggestion {
                            value: lit.to_string(),
                            span: Span::new(start, pos),
                            append_whitespace: true,
                            ..reedline::Suggestion::default()
                        })
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();

            reedline::CompletionResult::fresh(completions)
        }
    }

    // Let Reedline/Crossterm read terminal geometry from the OS TTY interface.
    // Do not probe the terminal with cursor-position escape sequences or force
    // guessed dimensions here: serial consoles may not expose their real size.
    let env = Env::default();

    let mode = Arc::new(Mutex::new(Mode::Operational));
    let completer = Box::new(NetcliCompleter { mode: mode.clone(), registry });
    // Use a columnar menu so short command candidates are shown side-by-side
    // on one row, like a network-appliance CLI. Twelve columns cover the common
    // command families; Reedline wraps only when the available terminal width
    // cannot fit all candidates. Reedline 0.52.1 also contains the upstream fix
    // for incorrect prompt movement when terminal height is zero or too small.
    let completion_menu = Box::new(
        ColumnarMenu::default()
            .with_name("completion_menu")
            .with_columns(12)
            .with_column_width(None)
            .with_column_padding(2)
            // Pass the full buffer prefix to the completer, not only text typed
            // after the menu opened. This keeps completion context-aware.
            .with_input_mode(InputMode::CursorPrefix),
    );
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    let edit_mode = Box::new(Emacs::new(keybindings));

    let mut line_editor = Reedline::create()
        .with_completer(completer)
        // Auto-insert a unique completion and extend to the longest common
        // prefix when candidates share one, matching conventional network CLIs.
        .with_quick_completions(true)
        .with_partial_completions(true)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(edit_mode);

    loop {
        let cur_mode = *mode.lock().unwrap();
        let prompt = DefaultPrompt::new(
            DefaultPromptSegment::Basic(session::prompt_str(cur_mode).trim_end().to_string()),
            DefaultPromptSegment::Empty,
        );
        match line_editor.read_line(&prompt) {
            Ok(Signal::Success(buffer)) => {
                let mut m = *mode.lock().unwrap();
                let step = process_line(registry, &mut m, &env, &mut io::stdout(), &buffer);
                *mode.lock().unwrap() = m;
                if step.flow == Flow::Quit {
                    break;
                }
            }
            Ok(Signal::CtrlD) | Ok(Signal::CtrlC) => break,
            // Signal is non-exhaustive; ignore future informational signals.
            Ok(_) => continue,
            Err(e) => {
                eprintln!("error: {e}");
                break;
            }
        }
    }
}
