//! Linux backend of rf_win::bench. Status: STUB

pub fn machine(_test_dir: &Path) -> Machine {
    stub!("bench::machine")
}

/// Runs `f` (its result is real), but nothing is measured: every number is NaN, not a time of 0 ms.
pub fn measure_in_process<T>(f: impl FnOnce() -> T) -> (T, InProcess) {
    // STUB(linux): InProcess has no Default; NaN says "not measured"
    stub::hit("bench::measure_in_process");
    (f(), InProcess { ms: f64::NAN, cpu_ms: f64::NAN, extra_mb: f64::NAN })
}
