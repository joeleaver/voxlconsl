//! `voxlconsl balance` — headless balance-sweep harness.
//!
//! Loads a .voxl, sets the cart's scenario override (so it boots into
//! the cart's `BALANCE_MODE_FLAG = true` path), drives `Cart::update`
//! + CA + bodies steps at a synthetic fixed dt with no rendering, and
//! captures the cart's CSV-formatted `log()` output via the host's
//! `set_log_callback` hook.
//!
//! MVP scope: no-op-player only (the cart's BALANCE_MODE branch
//! already suppresses input). Tier × seed sweeps run sequentially in
//! one process. Parallel + reactive-AI variants are a phase-3 follow.

use std::path::Path;
use std::sync::Mutex;

use voxlconsl_host::sandbox::{Cart, BALANCE_OVERRIDE};

/// Synthetic per-frame dt. Matches a normal browser play at ~60 fps.
/// Mission timer is `MISSION_DURATION_MS / DT_MS = 11250` frames for
/// the current 3:00 mission length, so `MAX_FRAMES = 12000` gives a
/// small buffer past the natural mission end.
///
/// Wasmi is fully interpreted natively; cart frames cost roughly
/// 30-50 ms wall time depending on fire intensity. A full 12_000-frame
/// run is therefore ~6 minutes wall time. Use `--max-frames` to cap
/// shorter for quick iteration; the cart still emits per-5-second
/// rows + a (possibly truncated) end summary at the cap.
const DT_MS: u32 = 16;
pub(crate) const DEFAULT_MAX_FRAMES: u32 = 12_000;

/// Captured log lines, in order. The `set_log_callback` hook is a
/// `fn(&str)` (not `Fn(...)`), so we route through a process-global
/// mutex. Single-threaded sweeps don't contend; future parallel
/// sweeps will need a thread-local sink instead.
static LOG_SINK: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn capture_log(msg: &str) {
    let mut g = LOG_SINK.lock().expect("LOG_SINK poisoned");
    g.push(msg.to_string());
}

/// One scenario run.
#[allow(dead_code)] // `tier` + `seed` are kept for future structured-CSV consumers
pub struct RunReport {
    pub tier:    u8,
    pub seed:    u32,
    /// All `[cart] ...` lines emitted during the run, in order.
    pub log:     Vec<String>,
    /// Frames elapsed before the cart emitted a BAL_END line (or
    /// `max_frames` if the cart never resolved).
    pub frames:  u32,
}

pub fn run_sweep(
    cart_path: &Path,
    tiers: &[u8],
    seeds: &[u32],
    out: Option<&Path>,
    max_frames: u32,
) -> Result<(), String> {
    let cart_bytes = std::fs::read(cart_path)
        .map_err(|e| format!("read {}: {e}", cart_path.display()))?;

    voxlconsl_host::sandbox::set_log_callback(capture_log);

    let mut all_lines: Vec<String> = Vec::new();
    let mut header_written = false;

    eprintln!(
        "voxlconsl balance: {} tier × {} seeds = {} runs",
        tiers.len(),
        seeds.len(),
        tiers.len() * seeds.len(),
    );

    let started = std::time::Instant::now();
    let mut completed = 0usize;

    for &tier in tiers {
        for &seed in seeds {
            let report = run_one(&cart_bytes, seed, tier, max_frames)?;
            completed += 1;
            eprintln!(
                "  [{:>3}/{:<3}] tier={} seed={:#010x} frames={} log_lines={}",
                completed, tiers.len() * seeds.len(),
                tier, seed, report.frames, report.log.len(),
            );

            for line in &report.log {
                // The cart's header is identical across runs — only
                // emit the first one to keep the combined CSV clean.
                let is_header = line.contains("BAL,t_s,tier,seed,fire");
                if is_header {
                    if !header_written {
                        all_lines.push(strip_prefix(line));
                        header_written = true;
                    }
                    continue;
                }
                // Drop the host's "[cart] " prefix so the CSV is clean.
                all_lines.push(strip_prefix(line));
            }
        }
    }

    let elapsed = started.elapsed();
    eprintln!(
        "voxlconsl balance: done in {:.2}s ({:.0} ms / run)",
        elapsed.as_secs_f64(),
        elapsed.as_millis() as f64 / completed.max(1) as f64,
    );

    let csv = all_lines.join("\n") + "\n";
    match out {
        Some(path) => {
            std::fs::write(path, csv).map_err(|e| format!("write {}: {e}", path.display()))?;
            eprintln!("wrote {} ({} lines)", path.display(), all_lines.len());
        }
        None => {
            print!("{csv}");
        }
    }

    Ok(())
}

fn run_one(cart_bytes: &[u8], seed: u32, tier: u8, max_frames: u32) -> Result<RunReport, String> {
    // Reset the per-process log sink so this run only captures its
    // own lines.
    LOG_SINK.lock().expect("LOG_SINK poisoned").clear();

    // Set the override before Cart::load — the cart's init() reads it
    // exactly once via the env.balance_get_scenario_override import.
    BALANCE_OVERRIDE.with(|c| c.set(Some((seed, tier))));

    let mut cart = Cart::load(cart_bytes)
        .map_err(|e| format!("cart load failed: {e:?}"))?;

    let dt_s = DT_MS as f32 / 1000.0;
    let mut frames = 0u32;
    let mut resolved = false;
    let started = std::time::Instant::now();
    while frames < max_frames {
        cart.update(DT_MS)
            .map_err(|e| format!("cart update failed at frame {frames}: {e:?}"))?;
        cart.render()
            .map_err(|e| format!("cart render failed at frame {frames}: {e:?}"))?;
        cart.world().input.end_of_frame(DT_MS);
        voxlconsl_host::bodies::step(cart.world(), dt_s);
        voxlconsl_host::ca::tick(cart.world());
        frames += 1;

        if frames % 500 == 0 {
            eprintln!(
                "    frame {frames}/{max_frames} ({:.2}s wall, sink={} lines)",
                started.elapsed().as_secs_f64(),
                LOG_SINK.lock().expect("LOG_SINK poisoned").len(),
            );
        }
        // Check for BAL_END every ~half-second to keep overhead low.
        if frames % 30 == 0 {
            let sink = LOG_SINK.lock().expect("LOG_SINK poisoned");
            if let Some(last) = sink.last() {
                if last.contains("BAL_END,") {
                    resolved = true;
                    break;
                }
            }
        }
    }
    if !resolved {
        eprintln!(
            "  warning: tier={} seed={:#010x} did not resolve within {} frames",
            tier, seed, max_frames,
        );
    }

    let log = LOG_SINK.lock().expect("LOG_SINK poisoned").clone();
    Ok(RunReport { tier, seed, log, frames })
}

fn strip_prefix(line: &str) -> String {
    // Host prepends "[cart] " before the cart-supplied payload.
    line.strip_prefix("[cart] ").unwrap_or(line).to_string()
}

/// Parse a CSV/range spec like `"1,2,3"` or `"1..5"` or `"2"`. Returns
/// `Err` on garbage input. Endpoints in `start..end` are inclusive of
/// start, exclusive of end — same as Rust's `..` so `1..5` produces
/// `[1, 2, 3, 4]`.
pub fn parse_u32_list(spec: &str) -> Result<Vec<u32>, String> {
    if let Some((a, b)) = spec.split_once("..") {
        let start = parse_one_u32(a)?;
        let end   = parse_one_u32(b)?;
        if end <= start {
            return Err(format!("range '{spec}': end must exceed start"));
        }
        Ok((start..end).collect())
    } else {
        spec.split(',')
            .map(|s| parse_one_u32(s.trim()))
            .collect()
    }
}

pub fn parse_u8_list(spec: &str) -> Result<Vec<u8>, String> {
    parse_u32_list(spec)?
        .into_iter()
        .map(|v| u8::try_from(v).map_err(|_| format!("value {v} out of u8 range")))
        .collect()
}

fn parse_one_u32(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| format!("hex '{s}': {e}"))
    } else {
        s.parse::<u32>().map_err(|e| format!("decimal '{s}': {e}"))
    }
}
