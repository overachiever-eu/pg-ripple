//! Fuzz target for v0.135 registered-prefix prologue handling.
//!
//! The production resolver only prepends missing declarations. This harness
//! checks that formal-prologue scanning is panic-free and never rewrites the
//! original query body or local declarations.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(query) = std::str::from_utf8(data) else {
        return;
    };
    pg_ripple_fuzz::check_prefix_prologue(query);
});
