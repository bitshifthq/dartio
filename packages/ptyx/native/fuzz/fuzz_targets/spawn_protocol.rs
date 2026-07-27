#![no_main]
#![allow(dead_code)]

use libfuzzer_sys::fuzz_target;

#[path = "../../broker/src/main.rs"]
mod broker;

fuzz_target!(|payload: &[u8]| {
    broker::fuzz_spawn_payload(payload);
});
