//! CLI core: command model, parser, and completion engine.
pub mod command;
pub mod completion;
pub mod parser;

// Only re-export the pieces used by the session layer. Keep internal helpers
// accessed through their owning modules so the binary does not get unused imports.
pub use command::{CommandRegistry, Mode};
pub use completion::help_entries;
pub use parser::{match_line, MatchError};
