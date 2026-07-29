#![no_main]
#![allow(dead_code)]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "../../broker/src/main.rs"]
mod broker;

fuzz_target!(|bytes: &[u8]| {
    broker::fuzz_protocol_frame(bytes);
});
