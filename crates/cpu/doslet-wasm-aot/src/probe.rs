//! Native harness for the trusted standalone game probe ABI.
//! Both interpreter/runtime and translated guest blocks run inside Wasmtime.
use super::Artifact;
use doslet_x86_wasm::STATE_BYTES;
use wasmtime::{Caller, Linker, Memory, Store, TypedFunc};

pub struct GameArtifacts {
    pub probe: Artifact,
    pub translated: Artifact,
}
impl GameArtifacts {
    pub fn compile(probe: &[u8], translated: &[u8]) -> Result<Self, String> {
        let probe = Artifact::compile(probe)?;
        let translated = Artifact::compile_on(&probe.engine, translated)?;
        Ok(Self { probe, translated })
    }
    pub fn instantiate(&self, bundle: &[u8], translated: bool) -> Result<GameRunner, String> {
        let mut store = Store::new(
            &self.probe.engine,
            Bridge {
                memory: None,
                poll: None,
                handle: 0,
                scratch: 0,
                buffer: vec![0; STATE_BYTES].into_boxed_slice(),
            },
        );
        let mut linker = Linker::new(&self.probe.engine);
        linker
            .func_wrap(
                "doslet",
                "poll",
                |mut caller: Caller<'_, Bridge>,
                 completed: u32,
                 remaining: u32|
                 -> wasmtime::Result<i32> {
                    let module_memory = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .expect("translated memory");
                    let probe_memory = caller.data().memory.expect("probe memory");
                    let offset = caller.data().scratch as usize;
                    {
                        let (bytes, state) = module_memory.data_and_store_mut(&mut caller);
                        state.buffer.copy_from_slice(&bytes[..STATE_BYTES]);
                    }
                    {
                        let (bytes, state) = probe_memory.data_and_store_mut(&mut caller);
                        bytes[offset..offset + STATE_BYTES].copy_from_slice(&state.buffer);
                    }
                    let poll = caller.data().poll.clone().expect("probe poll");
                    let handle = caller.data().handle;
                    let result = poll.call(&mut caller, (handle, completed, remaining))?;
                    {
                        let (bytes, state) = probe_memory.data_and_store_mut(&mut caller);
                        state
                            .buffer
                            .copy_from_slice(&bytes[offset..offset + STATE_BYTES]);
                    }
                    {
                        let (bytes, state) = module_memory.data_and_store_mut(&mut caller);
                        bytes[..STATE_BYTES].copy_from_slice(&state.buffer);
                    }
                    Ok(result)
                },
            )
            .map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate(&mut store, &self.probe.module)
            .map_err(|e| e.to_string())?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or("probe memory")?;
        let input = instance
            .get_typed_func::<u32, u32>(&mut store, "probe_input")
            .map_err(|e| e.to_string())?;
        let ptr = input
            .call(
                &mut store,
                u32::try_from(bundle.len()).map_err(|_| "large bundle")?,
            )
            .map_err(|e| e.to_string())?;
        memory
            .write(&mut store, ptr as usize, bundle)
            .map_err(|e| e.to_string())?;
        let new = instance
            .get_typed_func::<(u32, u32), u32>(&mut store, "probe_new")
            .map_err(|e| e.to_string())?;
        let handle = new
            .call(&mut store, (ptr, bundle.len() as u32))
            .map_err(|e| format!("probe setup: {e:?}"))?;
        let scratch = instance
            .get_typed_func::<u32, u32>(&mut store, "probe_scratch")
            .map_err(|e| e.to_string())?
            .call(&mut store, handle)
            .map_err(|e| e.to_string())?;
        let poll = instance
            .get_typed_func::<(u32, u32, u32), i32>(&mut store, "probe_poll")
            .map_err(|e| e.to_string())?;
        store.data_mut().memory = Some(memory);
        store.data_mut().poll = Some(poll);
        store.data_mut().handle = handle;
        store.data_mut().scratch = scratch;
        let run_reference = instance
            .get_typed_func(&mut store, "probe_run")
            .map_err(|e| e.to_string())?;
        let reset = instance
            .get_typed_func(&mut store, "probe_reset")
            .map_err(|e| e.to_string())?;
        let output = instance
            .get_typed_func(&mut store, "probe_output")
            .map_err(|e| e.to_string())?;
        let drain = instance
            .get_typed_func(&mut store, "probe_drain")
            .map_err(|e| e.to_string())?;
        let run_translated = if translated {
            let generated = linker
                .instantiate(&mut store, &self.translated.module)
                .map_err(|e| e.to_string())?;
            Some(
                generated
                    .get_typed_func(&mut store, "run")
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let mut runner = GameRunner {
            store,
            memory,
            handle,
            reset,
            output,
            drain,
            run_reference,
            run_translated,
            failed: false,
        };
        if translated && runner.output(2)? != self.translated.source {
            return Err("WASM probe and translated module profiles differ".into());
        }
        Ok(runner)
    }
}
struct Bridge {
    memory: Option<Memory>,
    poll: Option<TypedFunc<(u32, u32, u32), i32>>,
    handle: u32,
    scratch: u32,
    buffer: Box<[u8]>,
}
pub struct GameRunner {
    store: Store<Bridge>,
    memory: Memory,
    handle: u32,
    reset: TypedFunc<(u32, u32), ()>,
    output: TypedFunc<(u32, u32), u64>,
    drain: TypedFunc<u32, ()>,
    run_reference: TypedFunc<(u32, u32), u32>,
    run_translated: Option<TypedFunc<u32, u32>>,
    failed: bool,
}
impl GameRunner {
    pub fn reset(&mut self, cache: bool) -> Result<(), String> {
        if self.failed {
            return Err("probe stopped after trap".into());
        }
        self.reset
            .call(&mut self.store, (self.handle, u32::from(cache)))
            .map_err(|e| e.to_string())
    }
    pub fn run(&mut self, budget: u32) -> Result<u32, String> {
        if self.failed {
            return Err("probe stopped after trap".into());
        }
        let result = if let Some(run) = &self.run_translated {
            run.call(&mut self.store, budget)
        } else {
            self.run_reference
                .call(&mut self.store, (self.handle, budget))
        };
        result.map_err(|e| {
            self.failed = true;
            format!("probe execution: {e:?}")
        })
    }
    pub fn drain(&mut self) -> Result<(), String> {
        self.drain
            .call(&mut self.store, self.handle)
            .map_err(|e| e.to_string())
    }
    pub fn output(&mut self, kind: u32) -> Result<Vec<u8>, String> {
        let packed = self
            .output
            .call(&mut self.store, (self.handle, kind))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; (packed >> 32) as usize];
        self.memory
            .read(&self.store, packed as u32 as usize, &mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}
