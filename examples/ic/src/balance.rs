//! Balance instrumentation. When `lib::BALANCE_MODE` is on, the cart
//! emits CSV-friendly rows via `log()` describing what's happening on
//! the fire side of the world. Player input is suppressed in the
//! caller (see `lib.rs::update`) so the log captures a no-op-player
//! baseline: how does the fire alone evolve against a passive town?
//!
//! Format:
//! - One header line at boot: `BAL,t_s,tier,seed,fire,alive,mask,wind,str,h_b,h_t,c_b,c_t,hs_b,hs_t,e_b,e_t,q`
//! - One row every `BALANCE_LOG_INTERVAL_MS` of mission-elapsed time.
//! - One `BAL_FIRSTLOSS` line the tick the first cabin dies.
//! - One `BAL_END` line when the mission resolves.
//!
//! Read the browser console after a run; copy the `BAL*` lines into a
//! CSV. Pre-pend `outcome,tier,seed` per row if concatenating multiple
//! runs.

use voxlconsl_sdk::log;

/// Emit a row every N ms of mission time. 5 s × 36 rows / 180 s
/// mission is plenty of resolution for trend-watching.
pub(crate) const BALANCE_LOG_INTERVAL_MS: u32 = 5_000;

pub(crate) struct BalanceMetrics {
    pub elapsed_ms: u32,
    pub tier:        u32,
    pub seed:        u32,
    pub fire_sites:  u32,
    pub alive_count: u32,
    pub alive_mask:  u32,
    pub wind_dir:    [u8; 2],
    pub wind_str:    u32,
    pub heli_busy:   u32,
    pub heli_total:  u32,
    pub crew_busy:   u32,
    pub crew_total:  u32,
    pub hs_busy:     u32,
    pub hs_total:    u32,
    pub eng_busy:    u32,
    pub eng_total:   u32,
    pub queue_pending: u32,
}

pub(crate) struct BalanceLog {
    /// Mission-elapsed-ms of the most recent row emit, or `u32::MAX`
    /// for "never emitted yet" (so the t=0 baseline row fires once).
    /// After the first emit this is always a real timestamp; the next
    /// emit fires when `elapsed >= last + INTERVAL`.
    last_log_ms: u32,
    /// Mission-elapsed-ms when the first cabin died, or u32::MAX if
    /// none yet.
    first_loss_ms: u32,
    /// Track per-row to detect a drop.
    last_alive: u32,
    /// Latched once the mission ends.
    end_logged: bool,
}

impl BalanceLog {
    pub(crate) const fn new() -> Self {
        Self {
            last_log_ms: u32::MAX,
            first_loss_ms: u32::MAX,
            last_alive: 6,
            end_logged: false,
        }
    }

    /// Emit the column header. Call once at boot when BALANCE_MODE is on.
    pub(crate) fn emit_header(&self) {
        log("BAL,t_s,tier,seed,fire,alive,mask,wind,str,h_b,h_t,c_b,c_t,hs_b,hs_t,e_b,e_t,q");
    }

    /// Call every frame while the mission is Playing. Emits a CSV row
    /// when the interval elapses, plus a tagged FIRSTLOSS line on the
    /// frame a cabin first burns down.
    pub(crate) fn tick(&mut self, m: &BalanceMetrics) {
        // First call after init: emit a baseline row so the CSV starts
        // at t=0 even if the run is short. After that, emit on the
        // configured interval.
        let due = if self.last_log_ms == u32::MAX {
            true
        } else {
            m.elapsed_ms >= self.last_log_ms + BALANCE_LOG_INTERVAL_MS
        };
        if due {
            self.emit_row("BAL", m);
            self.last_log_ms = m.elapsed_ms;
        }

        // Edge: first cabin loss.
        if self.first_loss_ms == u32::MAX && m.alive_count < 6 {
            self.first_loss_ms = m.elapsed_ms;
            self.emit_row("BAL_FIRSTLOSS", m);
        }
        self.last_alive = m.alive_count;
    }

    /// Call when the mission resolves. `outcome` is `"WON"` / `"LOST"`.
    /// Emits a single summary line and latches; subsequent calls no-op.
    pub(crate) fn emit_end(&mut self, outcome: &str, m: &BalanceMetrics) {
        if self.end_logged { return; }
        self.end_logged = true;

        // BAL_END,outcome,t_s,tier,seed,first_loss_s,alive,mask
        let mut buf = [0u8; 128];
        let mut len = 0usize;
        len = push_str(&mut buf, len, "BAL_END,");
        len = push_str(&mut buf, len, outcome);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.elapsed_ms / 1000);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.tier);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.seed);
        len = push_str(&mut buf, len, ",");
        if self.first_loss_ms == u32::MAX {
            len = push_str(&mut buf, len, "-");
        } else {
            len = push_u32(&mut buf, len, self.first_loss_ms / 1000);
        }
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.alive_count);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.alive_mask);
        emit_buf(&buf, len);
    }

    fn emit_row(&self, tag: &str, m: &BalanceMetrics) {
        let mut buf = [0u8; 192];
        let mut len = 0usize;

        len = push_str(&mut buf, len, tag);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.elapsed_ms / 1000);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.tier);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.seed);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.fire_sites);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.alive_count);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.alive_mask);
        len = push_str(&mut buf, len, ",");
        // Wind dir — 1 or 2 ASCII chars, already space-padded.
        for &b in m.wind_dir.iter() {
            if b != b' ' && len < buf.len() {
                buf[len] = b;
                len += 1;
            }
        }
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.wind_str);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.heli_busy);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.heli_total);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.crew_busy);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.crew_total);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.hs_busy);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.hs_total);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.eng_busy);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.eng_total);
        len = push_str(&mut buf, len, ",");
        len = push_u32(&mut buf, len, m.queue_pending);

        emit_buf(&buf, len);
    }
}

// ── Tiny no_std-friendly formatting helpers ──────────────────────────

fn push_str(buf: &mut [u8], mut len: usize, s: &str) -> usize {
    for &b in s.as_bytes() {
        if len >= buf.len() { return len; }
        buf[len] = b;
        len += 1;
    }
    len
}

/// Decimal u32. Up to 10 digits. Returns new len.
fn push_u32(buf: &mut [u8], mut len: usize, mut n: u32) -> usize {
    if n == 0 {
        if len < buf.len() { buf[len] = b'0'; len += 1; }
        return len;
    }
    let mut digits = [0u8; 10];
    let mut d = 0usize;
    while n > 0 {
        digits[d] = b'0' + (n % 10) as u8;
        n /= 10;
        d += 1;
    }
    while d > 0 {
        d -= 1;
        if len >= buf.len() { return len; }
        buf[len] = digits[d];
        len += 1;
    }
    len
}

fn emit_buf(buf: &[u8], len: usize) {
    if let Ok(s) = core::str::from_utf8(&buf[..len]) {
        log(s);
    }
}
