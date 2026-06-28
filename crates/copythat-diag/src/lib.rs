//! Phase 47 — "Why is this slow?" diagnostics.
//!
//! Pure analysis surface for explaining throughput dips during a copy: a
//! per-window [`PhaseSample`], a [`Bottleneck`] classifier, and
//! [`annotate_dips`] which turns a window series into [`SpeedDip`]s the UI
//! can pin onto the live speed graph — each tagged with a `cause_emoji`.
//!
//! ## What ships here
//!
//! - The stable public types ([`Bottleneck`], [`PhaseSample`],
//!   [`SpeedDip`]) — serde-round-trippable so the Tauri layer can carry
//!   them over IPC without a bespoke schema.
//! - Real, testable analysis: [`classify`] (a dominant-wait heuristic with
//!   explicit thermal + antivirus overrides) and [`annotate_dips`].
//! - The [`DiagSink`] hook + [`NoopDiagSink`], mirroring the
//!   `Option<Arc<dyn …Sink>>` slots `copythat-core` already exposes for
//!   shape / journal / chunk.
//!
//! ## Deferred (next increment)
//!
//! The per-syscall instrumentation that POPULATES a [`PhaseSample`] (the
//! read / write / cpu / net wait timing) inside `copythat-core`'s copy
//! loop, and the Svelte speed-graph rendering of [`SpeedDip`]s, are NOT
//! wired yet. This crate ships the stable types + classifier first so that
//! wiring can land without churning the public API — the same
//! scaffold-first pattern Phase 46's `copythat-plugin` used.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

/// What was holding a copy back during a measurement window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bottleneck {
    /// Reading from the source volume dominated the window.
    SourceIo,
    /// Writing to the destination volume dominated the window.
    DestIo,
    /// A network leg (SMB / cloud / SFTP) dominated the window.
    Network,
    /// A real-time antivirus scanner stalled the reads (a source-IO stall
    /// coinciding with a known-AV signal).
    Antivirus,
    /// The CPU was the limiter (hashing / compression / encryption).
    Cpu,
    /// Thermal throttling (CPU or drive controller) capped throughput.
    Thermal,
    /// No single cause dominated (or the window carried no timing).
    Unknown,
}

impl Bottleneck {
    /// A single glyph the speed graph pins on an annotated dip.
    pub fn cause_emoji(&self) -> &'static str {
        match self {
            Bottleneck::SourceIo => "📥",
            Bottleneck::DestIo => "📤",
            Bottleneck::Network => "🌐",
            Bottleneck::Antivirus => "🛡",
            Bottleneck::Cpu => "🧮",
            Bottleneck::Thermal => "🌡",
            Bottleneck::Unknown => "❔",
        }
    }

    /// Human-readable label (English source; the UI localizes via Fluent).
    pub fn label(&self) -> &'static str {
        match self {
            Bottleneck::SourceIo => "Source I/O",
            Bottleneck::DestIo => "Destination I/O",
            Bottleneck::Network => "Network",
            Bottleneck::Antivirus => "Antivirus",
            Bottleneck::Cpu => "CPU",
            Bottleneck::Thermal => "Thermal",
            Bottleneck::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for Bottleneck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One measurement window of a copy: how long it took, how many bytes
/// moved, and where the time went. The `*_wait_ns` fields are wall-clock
/// nanoseconds spent blocked on each resource; `cpu_busy_ns` is time spent
/// on CPU work (hash / compress / encrypt). The boolean signals
/// (`av_suspected`, `thermal_throttling`) are best-effort flags the
/// platform layer sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PhaseSample {
    /// Wall-clock duration of the window.
    pub elapsed_ns: u64,
    /// Bytes copied during the window.
    pub bytes: u64,
    /// Nanoseconds blocked reading the source.
    pub read_wait_ns: u64,
    /// Nanoseconds blocked writing the destination.
    pub write_wait_ns: u64,
    /// Nanoseconds blocked on a network leg.
    pub net_wait_ns: u64,
    /// Nanoseconds spent busy on CPU work (hash / compress / encrypt).
    pub cpu_busy_ns: u64,
    /// A known real-time AV scanner was active on the path this window.
    pub av_suspected: bool,
    /// Thermal throttling (CPU or drive controller) was detected.
    pub thermal_throttling: bool,
}

impl PhaseSample {
    /// Throughput in bytes/second for this window (`0.0` for a
    /// zero-duration window).
    pub fn throughput_bytes_per_s(&self) -> f64 {
        if self.elapsed_ns == 0 {
            return 0.0;
        }
        self.bytes as f64 / (self.elapsed_ns as f64 / 1_000_000_000.0)
    }
}

/// An annotated drop on the speed graph: when it happened, how slow it
/// got, and the classified cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedDip {
    /// Milliseconds from the start of the copy.
    pub at_ms: u64,
    /// Throughput at the dip, bytes/second.
    pub throughput_bytes_per_s: u64,
    /// Classified cause of the dip.
    pub cause: Bottleneck,
}

/// Classify what bottlenecked a single window.
///
/// Explicit signals win first (a thermal throttle, then an AV scanner
/// masquerading as a source-IO stall); otherwise the resource with the
/// dominant wait (more than half the accounted time) is the cause. A
/// window with no clear majority — or no recorded timing — is `Unknown`.
pub fn classify(sample: &PhaseSample) -> Bottleneck {
    if sample.thermal_throttling {
        return Bottleneck::Thermal;
    }
    let waits = [
        (Bottleneck::SourceIo, sample.read_wait_ns),
        (Bottleneck::DestIo, sample.write_wait_ns),
        (Bottleneck::Network, sample.net_wait_ns),
        (Bottleneck::Cpu, sample.cpu_busy_ns),
    ];
    let total: u64 = waits.iter().map(|&(_, ns)| ns).sum();
    if total == 0 {
        return Bottleneck::Unknown;
    }
    let (dominant, dom_ns) = waits
        .iter()
        .copied()
        .max_by_key(|&(_, ns)| ns)
        .expect("waits is never empty");
    // Require a clear majority before attributing a single cause.
    if dom_ns.saturating_mul(2) <= total {
        return Bottleneck::Unknown;
    }
    // A read-dominated window with a known AV signal is the scanner, not
    // the disk itself.
    if dominant == Bottleneck::SourceIo && sample.av_suspected {
        return Bottleneck::Antivirus;
    }
    dominant
}

/// Walk a window series and flag every window whose throughput fell below
/// `dip_fraction` of the rolling baseline (the best throughput seen so
/// far), tagging each dip with [`classify`]. `dip_fraction` is clamped to
/// `0.0..=1.0`.
pub fn annotate_dips(samples: &[PhaseSample], dip_fraction: f64) -> Vec<SpeedDip> {
    let frac = dip_fraction.clamp(0.0, 1.0);
    let mut dips = Vec::new();
    let mut baseline = 0.0_f64;
    let mut at_ms: u64 = 0;
    for sample in samples {
        let tput = sample.throughput_bytes_per_s();
        if tput > baseline {
            baseline = tput;
        }
        if baseline > 0.0 && tput < baseline * frac {
            dips.push(SpeedDip {
                at_ms,
                throughput_bytes_per_s: tput as u64,
                cause: classify(sample),
            });
        }
        at_ms = at_ms.saturating_add(sample.elapsed_ns / 1_000_000);
    }
    dips
}

/// Engine → diagnostics hook, called once per measurement window so a
/// consumer (the Tauri shell's live speed graph) can record samples and
/// their classification.
///
/// DEFERRED: `copythat-core`'s copy loop does not yet emit samples (the
/// per-syscall read / write / cpu / net timing that fills a
/// [`PhaseSample`] is the next increment), and no UI renders them. The
/// trait is shipped now so that wiring slots in without changing this API
/// — mirroring the `Option<Arc<dyn …Sink>>` hooks `copythat-core` already
/// takes for shape / journal / chunk.
pub trait DiagSink: Send + Sync {
    /// Record one classified window.
    fn on_sample(&self, sample: &PhaseSample, classification: Bottleneck);
}

/// No-op sink — diagnostics disabled (the default).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDiagSink;

impl DiagSink for NoopDiagSink {
    fn on_sample(&self, _sample: &PhaseSample, _classification: Bottleneck) {}
}
