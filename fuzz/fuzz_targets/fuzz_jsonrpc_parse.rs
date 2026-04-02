#![no_main]

use libfuzzer_sys::fuzz_target;

/// Feed arbitrary bytes into the JSON-RPC parser.
///
/// A panic here means the parser is not robust to malformed input — it must
/// return an `Err`, never abort, never loop infinitely, and never allocate
/// unboundedly. The fuzzer explores all reachable code paths automatically.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The return value is deliberately ignored: we only care that no panic occurs.
        let _ = agentgate_core::protocol::jsonrpc::JsonRpcMessage::parse(s);
    }
});
