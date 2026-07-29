#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    ptyx::__fuzzing::broker_protocol_frame(bytes);
});
