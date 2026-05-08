//! The `voxlconsl` CLI. See SPEC.md §12.4.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "voxlconsl", about = "voxlconsl fantasy console toolchain")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Scaffold a starter cart project.
    New {
        name: String,
    },
    /// Build .wasm, gather assets, write a .voxl.
    Bundle {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Launch a .voxl in the desktop reference host.
    Run {
        cart: PathBuf,
    },
    /// Dev mode: rebuild and reload in the browser host on file change.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Lint a .voxl: section sizes, palette references, schema validity.
    Validate {
        cart: PathBuf,
    },
    /// Convert a foreign voxel format (e.g. .vox) to .vxv.
    Import {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        colors: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::New { name } => {
            eprintln!("voxlconsl new {name}: not yet implemented");
        }
        Command::Bundle { path } => {
            eprintln!("voxlconsl bundle {}: not yet implemented", path.display());
        }
        Command::Run { cart } => {
            eprintln!("voxlconsl run {}: not yet implemented", cart.display());
        }
        Command::Serve { path } => {
            eprintln!("voxlconsl serve {}: not yet implemented", path.display());
        }
        Command::Validate { cart } => {
            eprintln!("voxlconsl validate {}: not yet implemented", cart.display());
        }
        Command::Import { input, output, colors } => {
            eprintln!(
                "voxlconsl import {} -> {} (colors: {:?}): not yet implemented",
                input.display(),
                output.display(),
                colors,
            );
        }
    }
}
