use ptyx::{Size, SpawnOptions};
use std::ffi::OsString;

#[test]
fn command_builder_preserves_owned_spawn_values() {
    let executable = OsString::from("/bin/sh");
    let argument = OsString::from("printf ready");
    let options = SpawnOptions::new(executable.clone())
        .argument("-c")
        .argument(argument.clone())
        .size(Size::new(40, 120));

    assert_eq!(options.executable(), executable);
    assert_eq!(
        options.arguments(),
        [OsString::from("-c"), argument].as_slice()
    );
    assert_eq!(options.initial_size(), Size::new(40, 120));
}

#[test]
fn size_rejects_zero_cell_dimensions() {
    let error = Size::try_new(0, 80).unwrap_err();

    assert_eq!(
        error.to_string(),
        "terminal rows and columns must be in 1..=32767"
    );
}

#[test]
fn size_rejects_nonportable_cell_dimensions() {
    let error = Size::try_new(32768, 80).unwrap_err();

    assert_eq!(
        error.to_string(),
        "terminal rows and columns must be in 1..=32767"
    );
}
