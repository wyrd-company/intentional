// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "itentional",
    version,
    about = "Intent-driven polyglot releases"
)]
struct Cli {}

fn main() {
    Cli::parse();
}
