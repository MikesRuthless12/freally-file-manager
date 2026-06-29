//! Phase 47 increment 2 — live system-resource sampling for
//! [`DiagSnapshot`](crate::DiagSnapshot).
//!
//! [`SystemSampler`] turns the cross-platform metrics the diagnostics
//! classifier needs into a [`DiagSnapshot`]. It holds a `sysinfo::System`
//! and refreshes it per call, so CPU usage is a delta since the previous
//! sample — call it on a steady cadence (the Phase 47 spec's 1 Hz).
//!
//! **Cross-platform today:** overall CPU utilisation. **Deferred to the
//! next increment:** the per-OS disk-busy % (Windows WMI `% Disk Time`,
//! Linux `/proc/diskstats`, macOS `iostat`), network RTT / loss, and
//! antivirus-scan detection — those [`DiagSnapshot`] fields stay `None`
//! until their per-OS samplers land, the same scaffold-first staging the
//! rest of this crate uses.

use sysinfo::System;

use crate::DiagSnapshot;

/// Stateful sampler for the system metrics that feed a [`DiagSnapshot`].
pub struct SystemSampler {
    sys: System,
}

impl SystemSampler {
    /// Create a sampler with a primed CPU baseline. The first
    /// [`cpu_pct`](Self::cpu_pct) reads a delta from this baseline, so it
    /// needs at least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`] to elapse
    /// for a meaningful value (it returns `0.0` if called immediately).
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self { sys }
    }

    /// Overall CPU utilisation percent (`0.0..=100.0`) since the previous
    /// refresh. Refreshes CPU usage as a side effect, so successive calls
    /// measure the interval between them.
    pub fn cpu_pct(&mut self) -> f32 {
        self.sys.refresh_cpu_usage();
        self.sys.global_cpu_usage()
    }

    /// Build a [`DiagSnapshot`] for this instant from the copy's measured
    /// `instant_throughput` (bytes/s) and the caller-supplied
    /// `thermal_throttling` flag (the Phase 31 power probe), plus the live
    /// CPU reading. Per-OS disk / network / antivirus fields are left
    /// unset until their samplers land.
    pub fn snapshot(&mut self, instant_throughput: f64, thermal_throttling: bool) -> DiagSnapshot {
        DiagSnapshot {
            instant_throughput,
            cpu_pct: self.cpu_pct(),
            thermal_throttling,
            ..DiagSnapshot::default()
        }
    }
}

impl Default for SystemSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_pct_is_a_valid_percent() {
        let mut s = SystemSampler::new();
        let pct = s.cpu_pct();
        assert!(
            (0.0..=100.0).contains(&pct),
            "cpu_pct outside 0..=100: {pct}"
        );
    }

    #[test]
    fn snapshot_carries_throughput_thermal_and_defers_per_os_fields() {
        let mut s = SystemSampler::new();
        let snap = s.snapshot(1234.0, true);
        assert_eq!(snap.instant_throughput, 1234.0);
        assert!(snap.thermal_throttling);
        // Per-OS fields are deferred → unset.
        assert!(snap.src_disk_busy_pct.is_none());
        assert!(snap.dst_disk_busy_pct.is_none());
        assert!(snap.av_scan_active.is_none());
        assert!((0.0..=100.0).contains(&snap.cpu_pct));
    }
}
