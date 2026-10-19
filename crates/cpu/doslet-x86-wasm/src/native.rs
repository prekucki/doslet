//! Optional native WASM interpreter for differential testing, not a speed claim.
extern crate std;
use crate::host::{Host, Runtime};
use std::{format, string::String};
use wasmi::{
    Caller, Engine, Global, Linker, Memory, MemoryType, Module, Mutability, Store, TypedFunc, Val,
};

struct State<R: Runtime> {
    host: Host<R>,
    memory: Option<Memory>,
    error: Option<crate::host::Error<R::Error>>,
}
pub struct Runner<R: Runtime> {
    store: Store<State<R>>,
    run: TypedFunc<u32, u32>,
    _memory: Memory,
    failed: bool,
}
impl<R: Runtime + 'static> Runner<R> {
    pub fn new(host: Host<R>) -> Result<Self, String> {
        let engine = Engine::default();
        let module = Module::new(&engine, host.translation().wasm()).map_err(|e| format!("{e}"))?;
        let mut store = Store::new(
            &engine,
            State {
                host,
                memory: None,
                error: None,
            },
        );
        // This runner owns the scratch memory; its base is zero.
        let pages = crate::STATE_BYTES.div_ceil(65536) as u32;
        let memory = Memory::new(&mut store, MemoryType::new(pages, Some(pages)))
            .map_err(|e| format!("{e}"))?;
        store.data_mut().memory = Some(memory);
        let base = Global::new(&mut store, Val::I32(0), Mutability::Const);
        let mut linker = Linker::new(&engine);
        linker
            .define("doslet", "memory", memory)
            .and_then(|linker| linker.define("doslet", "base", base))
            .map_err(|e| format!("{e}"))?;
        linker
            .func_wrap(
                "doslet",
                "poll",
                |mut caller: Caller<'_, State<R>>, completed: u32, remaining: u32| -> i32 {
                    let memory = caller.data().memory.expect("runner memory");
                    let (bytes, state) = memory.data_and_store_mut(&mut caller);
                    match state.host.poll(completed, remaining, bytes) {
                        Ok(id) => id,
                        Err(error) => {
                            state.error = Some(error);
                            -2
                        }
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| format!("{e}"))?;
        let run = instance
            .get_typed_func::<u32, u32>(&store, "run")
            .map_err(|e| format!("{e}"))?;
        Ok(Self {
            store,
            run,
            _memory: memory,
            failed: false,
        })
    }
    pub fn host(&self) -> &Host<R> {
        &self.store.data().host
    }
    pub fn host_mut(&mut self) -> &mut Host<R> {
        &mut self.store.data_mut().host
    }
    pub fn run(&mut self, budget: u32) -> Result<u32, String>
    where
        R::Error: core::fmt::Debug,
    {
        if self.failed || self.store.data().error.is_some() {
            return Err("WASM runner stopped after a runtime error".into());
        }
        let result = match self.run.call(&mut self.store, budget) {
            Ok(result) => result,
            Err(error) => {
                self.failed = true;
                return Err(format!("WASM execution: {error}"));
            }
        };
        if let Some(e) = &self.store.data().error {
            return Err(format!("WASM runtime: {e:?}"));
        }
        Ok(result)
    }
}
