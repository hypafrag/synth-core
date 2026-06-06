//! synth-core — core library of the synth modular synthesizer platform.
//!
//! See the `synth` dev repo (`docs/architecture/`) for the design.

pub mod audio;
pub mod engine;
pub mod model;
pub mod modules;
pub mod processing;
pub mod registry;

/// Prints a greeting to stdout. Used to smoke-test cross-crate wiring:
/// the `synth-cli` and `synth-ui` binaries call this and exit.
pub fn synth_hello_world() {
    print_hello(&mut std::io::stdout()).expect("failed to write greeting to stdout");
}

/// Writes the greeting to `w`. Split out from [`synth_hello_world`] so the output is
/// testable without capturing the process's stdout.
fn print_hello<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Hello World!!! I'm synth!!!")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_matches() {
        let mut buf = Vec::new();
        print_hello(&mut buf).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "Hello World!!! I'm synth!!!\n"
        );
    }
}
