//! The `voxlconsl` CLI. See SPEC.md §12.4.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

mod balance;

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
    /// Headless balance sweep — load a .voxl, run it with no player
    /// input across a list of (tier, seed) pairs, dump the cart's
    /// CSV-formatted log lines combined to stdout or a file.
    ///
    /// Example: `voxlconsl balance --cart web/carts/ic.voxl \
    /// --tiers 1..5 --seeds 0..20 --out runs.csv`.
    Balance {
        /// Path to the cart .voxl to sweep.
        #[arg(long)]
        cart: PathBuf,
        /// Comma list or `start..end` range of tier values to sweep.
        /// Defaults to the cart's compile-time tier.
        #[arg(long, default_value = "2")]
        tiers: String,
        /// Comma list or `start..end` range of seeds. Accepts decimal
        /// or `0x`-prefixed hex. Defaults to a single small seed.
        #[arg(long, default_value = "0xA1F05E57")]
        seeds: String,
        /// Optional output CSV path. Without it the combined CSV
        /// prints to stdout (header once, rows from each run).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Cap on per-run frames before forcing termination. Default
        /// 12_000 (≈ a full 3:00 mission at 16 ms / frame). Lower for
        /// faster iteration when only the early phase matters — wasmi
        /// is interpreted natively and a full mission is ~5-6 min wall.
        #[arg(long, default_value_t = balance::DEFAULT_MAX_FRAMES)]
        max_frames: u32,
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
        Command::Balance { cart, tiers, seeds, out, max_frames } => {
            if let Err(e) = run_balance(&cart, &tiers, &seeds, out.as_deref(), max_frames) {
                eprintln!("voxlconsl balance: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn run_balance(
    cart: &Path,
    tiers_spec: &str,
    seeds_spec: &str,
    out: Option<&Path>,
    max_frames: u32,
) -> Result<(), String> {
    let tiers = balance::parse_u8_list(tiers_spec)
        .map_err(|e| format!("--tiers {tiers_spec}: {e}"))?;
    let seeds = balance::parse_u32_list(seeds_spec)
        .map_err(|e| format!("--seeds {seeds_spec}: {e}"))?;
    if tiers.is_empty() || seeds.is_empty() {
        return Err("at least one tier and one seed required".into());
    }
    balance::run_sweep(cart, &tiers, &seeds, out, max_frames)
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
