#![no_main]

use libfuzzer_sys::fuzz_target;

/// Feed arbitrary strings into the policy condition expression parser.
///
/// The condition parser handles hand-written TOML policy files from operators.
/// It must never panic on any input — invalid expressions must return `Err`.
/// The fuzzer explores partial expressions, deeply nested combinators, and
/// extremely long regex patterns to surface panics or unbounded allocations.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = agentgate_core::policy::condition::Expr::parse(s);
    }
});
