//! The `voxlconsl` CLI. See SPEC.md §12.4.

use std::path::{Path, PathBuf};

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
        /// Override the output path (default: <path>/target/<cart_name>.voxl).
        #[arg(short, long)]
        output: Option<PathBuf>,
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
        Command::Bundle { path, output } => {
            if let Err(e) = run_bundle(&path, output.as_deref()) {
                eprintln!("voxlconsl bundle: {e}");
                std::process::exit(1);
            }
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

fn run_bundle(path: &Path, output: Option<&Path>) -> Result<(), String> {
    let bytes = voxlconsl_bundler::bundle_cart(path).map_err(|e| e.to_string())?;
    // Default output: <path>/target/<cart_name>.voxl. We re-parse the
    // manifest just for the cart name so the bundler stays focused on
    // emitting bytes.
    let out_path = match output {
        Some(p) => p.to_path_buf(),
        None => {
            let manifest = std::fs::read_to_string(path.join("cart.toml"))
                .map_err(|e| format!("read cart.toml: {e}"))?;
            let value: toml::Value = toml::from_str(&manifest)
                .map_err(|e| format!("parse cart.toml: {e}"))?;
            let name = value
                .get("cart")
                .and_then(|c| c.get("name"))
                .and_then(|n| n.as_str())
                .ok_or_else(|| "cart.toml missing [cart].name".to_string())?;
            let target_dir = path.join("target");
            std::fs::create_dir_all(&target_dir)
                .map_err(|e| format!("mkdir target: {e}"))?;
            target_dir.join(format!("{name}.voxl"))
        }
    };
    std::fs::write(&out_path, &bytes).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    eprintln!("wrote {} ({} bytes)", out_path.display(), bytes.len());
    Ok(())
}
