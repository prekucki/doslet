//! Settings shared by build-time precompilation and runtime JIT compilation.
pub fn config() -> wasmtime::Config {
    let mut config = wasmtime::Config::new();
    config
        .cranelift_opt_level(wasmtime::OptLevel::Speed)
        .consume_fuel(false);
    config
}
