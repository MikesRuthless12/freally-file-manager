//! Phase 47 increment 2 — live system-resource sampling for
//! [`DiagSnapshot`](crate::DiagSnapshot).
//!
//! [`SystemSampler`] turns the cross-platform metrics the diagnostics
//! classifier needs into a [`DiagSnapshot`]. It holds a `sysinfo::System`
//! and refreshes it per call, so CPU usage is a delta since the previous
//! sample — call it on a steady cadence (the Phase 47 spec's 1 Hz).
//!
//! **Sampled here (cross-platform):** overall CPU utilisation, via
//! `sysinfo`. **Sampled externally and passed in:** per-volume disk-busy %
//! (the platform-specific PDH / `/proc/diskstats` reader lives in
//! `copythat-platform`, the one crate allowed `unsafe`; this crate stays
//! FFI-free and takes the result as a plain [`DiskBusy`]). **Still
//! deferred:** network RTT / loss and antivirus-scan detection — those
//! [`DiagSnapshot`] fields stay `None`; a reliable cheap "scan active"
//! signal isn't available, and per-tick process enumeration would perturb
//! the throughput we're measuring.

use sysinfo::System;

use crate::DiagSnapshot;

/// Externally-sampled per-volume disk-busy percentages (`0.0..=100.0`)
/// for the copy's source and destination volumes. The platform disk
/// sampler that produces these lives in `copythat-platform`; this crate
/// takes them as plain `Option`s so it carries no OS-specific dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DiskBusy {
    /// Busy percent of the source volume, or `None` if unsampled / network.
    pub src_pct: Option<f32>,
    /// Busy percent of the destination volume, or `None` if unsampled /
    /// network.
    pub dst_pct: Option<f32>,
}

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
    /// `instant_throughput` (bytes/s), the caller-supplied
    /// `thermal_throttling` flag (the Phase 31 power probe) and `disks`
    /// (the platform disk sampler), plus the live CPU reading. Network and
    /// antivirus fields stay unset (see the module docs).
    pub fn snapshot(
        &mut self,
        instant_throughput: f64,
        thermal_throttling: bool,
        disks: DiskBusy,
    ) -> DiagSnapshot {
        DiagSnapshot {
            instant_throughput,
            src_disk_busy_pct: disks.src_pct,
            dst_disk_busy_pct: disks.dst_pct,
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
    fn snapshot_carries_throughput_thermal_disks_and_defers_net_av() {
        let mut s = SystemSampler::new();
        let snap = s.snapshot(
            1234.0,
            true,
            DiskBusy {
                src_pct: Some(12.5),
                dst_pct: Some(96.0),
            },
        );
        assert_eq!(snap.instant_throughput, 1234.0);
        assert!(snap.thermal_throttling);
        // Disk fields propagate from the platform sampler.
        assert_eq!(snap.src_disk_busy_pct, Some(12.5));
        assert_eq!(snap.dst_disk_busy_pct, Some(96.0));
        // Network + antivirus stay deferred → unset.
        assert!(snap.network_rtt_ms.is_none());
        assert!(snap.av_scan_active.is_none());
        assert!((0.0..=100.0).contains(&snap.cpu_pct));
    }
}
