//! Phase 47 scaffold smoke: the public diagnostics surface compiles, serde
//! round-trips the JSON shapes the Tauri IPC layer will carry, the
//! classifier attributes the obvious cases, and `annotate_dips` flags a
//! clear drop. The per-syscall engine instrumentation + the UI
//! speed-graph rendering are deferred (see the crate docs).

use copythat_diag::{
    Bottleneck, DiagSink, NoopDiagSink, PhaseSample, SpeedDip, annotate_dips, classify,
};
use std::collections::HashSet;

const ALL: [Bottleneck; 7] = [
    Bottleneck::SourceIo,
    Bottleneck::DestIo,
    Bottleneck::Network,
    Bottleneck::Antivirus,
    Bottleneck::Cpu,
    Bottleneck::Thermal,
    Bottleneck::Unknown,
];

/// 1-second, 100 MB window with the given wait breakdown (ns).
fn sample(read: u64, write: u64, net: u64, cpu: u64) -> PhaseSample {
    PhaseSample {
        elapsed_ns: 1_000_000_000,
        bytes: 100_000_000,
        read_wait_ns: read,
        write_wait_ns: write,
        net_wait_ns: net,
        cpu_busy_ns: cpu,
        av_suspected: false,
        thermal_throttling: false,
    }
}

#[test]
fn bottleneck_round_trips_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&Bottleneck::SourceIo).unwrap(),
        "\"source_io\""
    );
    for b in ALL {
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<Bottleneck>(&json).unwrap(), b);
    }
}

#[test]
fn phase_sample_and_speed_dip_round_trip() {
    let s = sample(800_000_000, 100_000_000, 0, 50_000_000);
    assert_eq!(
        serde_json::from_str::<PhaseSample>(&serde_json::to_string(&s).unwrap()).unwrap(),
        s
    );
    let dip = SpeedDip {
        at_ms: 1200,
        throughput_bytes_per_s: 4_000_000,
        cause: Bottleneck::DestIo,
    };
    assert_eq!(
        serde_json::from_str::<SpeedDip>(&serde_json::to_string(&dip).unwrap()).unwrap(),
        dip
    );
}

#[test]
fn classify_attributes_the_dominant_wait() {
    assert_eq!(
        classify(&sample(900_000_000, 50_000_000, 0, 50_000_000)),
        Bottleneck::SourceIo
    );
    assert_eq!(
        classify(&sample(50_000_000, 900_000_000, 0, 50_000_000)),
        Bottleneck::DestIo
    );
    assert_eq!(
        classify(&sample(50_000_000, 50_000_000, 900_000_000, 0)),
        Bottleneck::Network
    );
    assert_eq!(
        classify(&sample(50_000_000, 50_000_000, 0, 900_000_000)),
        Bottleneck::Cpu
    );
}

#[test]
fn classify_handles_thermal_av_and_indeterminate() {
    let mut thermal = sample(900_000_000, 0, 0, 0);
    thermal.thermal_throttling = true;
    assert_eq!(classify(&thermal), Bottleneck::Thermal);

    let mut av = sample(900_000_000, 50_000_000, 0, 0);
    av.av_suspected = true;
    assert_eq!(classify(&av), Bottleneck::Antivirus);

    // No clear majority -> Unknown.
    assert_eq!(
        classify(&sample(250_000_000, 250_000_000, 250_000_000, 250_000_000)),
        Bottleneck::Unknown
    );
    // No timing at all -> Unknown.
    assert_eq!(classify(&sample(0, 0, 0, 0)), Bottleneck::Unknown);
}

#[test]
fn cause_emoji_is_nonempty_and_unique() {
    let mut seen = HashSet::new();
    for b in ALL {
        let emoji = b.cause_emoji();
        assert!(!emoji.is_empty(), "{b:?} has an empty emoji");
        assert!(seen.insert(emoji), "duplicate emoji for {b:?}");
        assert!(!b.label().is_empty(), "{b:?} has an empty label");
    }
}

#[test]
fn annotate_dips_flags_an_obvious_drop() {
    // Two fast windows (~1 GB/s) set the baseline, then a 10x-slower
    // write-bound stall, then recovery.
    let mut fast = sample(10_000_000, 10_000_000, 0, 0);
    fast.bytes = 1_000_000_000;
    let mut slow = sample(50_000_000, 900_000_000, 0, 0);
    slow.bytes = 10_000_000;
    let dips = annotate_dips(&[fast, fast, slow, fast], 0.5);
    assert_eq!(dips.len(), 1, "exactly the slow window dips: {dips:?}");
    assert_eq!(dips[0].cause, Bottleneck::DestIo);
}

#[test]
fn noop_diag_sink_satisfies_the_trait() {
    let sink = NoopDiagSink;
    sink.on_sample(&PhaseSample::default(), Bottleneck::Unknown);
    fn takes_sink(_s: &dyn DiagSink) {}
    takes_sink(&sink);
}
