use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use common::policy::{ArtifactVerification, ResourceLimits, TransformDef};
use common::traits::DataStream;
use futures::future::BoxFuture;
use futures::StreamExt;
use sha2::Digest;

#[derive(Clone)]
pub struct TransformEngine {
    engine: wasmtime::Engine,
    module_cache: dashmap::DashMap<String, wasmtime::Module>,
}

pub trait ModuleResolver: Send + Sync {
    fn load<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;
}

#[derive(Default, Clone)]
pub struct FileModuleResolver;

impl FileModuleResolver {
    fn read_file(path: &str) -> Result<Vec<u8>> {
        std::fs::read(path).with_context(|| format!("read wasm module at {path}"))
    }

    fn resolve_path(image: &str) -> Result<String> {
        if let Some(rest) = image.strip_prefix("file://") {
            let trimmed = rest.trim_start_matches('/');
            return Ok(trimmed.to_string());
        }

        if image.ends_with(".wasm") {
            return Ok(image.to_string());
        }

        anyhow::bail!("unsupported module URI: {image}")
    }
}

impl ModuleResolver for FileModuleResolver {
    fn load<'a>(&'a self, image: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            let path = Self::resolve_path(image)?;
            Self::read_file(&path)
        })
    }
}

struct StoreState {
    limits: wasmtime::StoreLimits,
    #[cfg(feature = "wasi")]
    wasi: wasmtime_wasi::preview1::WasiP1Ctx,
}

impl TransformEngine {
    pub fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).context("init wasmtime engine")?;
        Ok(Self {
            engine,
            module_cache: dashmap::DashMap::new(),
        })
    }

    pub fn with_default_resolver() -> Result<(Self, FileModuleResolver)> {
        Ok((Self::new()?, FileModuleResolver))
    }

    pub async fn execute_stream(
        &self,
        input: DataStream,
        def: TransformDef,
        resolver: &dyn ModuleResolver,
    ) -> Result<DataStream> {
        let module = self
            .load_module(&def.image, def.verification.as_ref(), resolver)
            .await?;
        self.execute_with_module(input, def, module).await
    }

    async fn load_module(
        &self,
        image: &str,
        verification: Option<&ArtifactVerification>,
        resolver: &dyn ModuleResolver,
    ) -> Result<wasmtime::Module> {
        let cache_key = cache_key(image, verification);
        if let Some(module) = self.module_cache.get(&cache_key) {
            return Ok(module.clone());
        }

        let bytes = resolver.load(image).await?;
        verify_artifact(verification, &bytes)?;

        let module = wasmtime::Module::new(&self.engine, bytes).context("compile wasm module")?;
        self.module_cache.insert(cache_key, module.clone());
        Ok(module)
    }

    async fn execute_with_module(
        &self,
        input: DataStream,
        def: TransformDef,
        module: wasmtime::Module,
    ) -> Result<DataStream> {
        let limits = build_limits(&def.resources);

        #[cfg(feature = "wasi")]
        let state = StoreState {
            limits,
            wasi: build_wasi(&def.args)?,
        };

        #[cfg(not(feature = "wasi"))]
        let state = StoreState { limits };

        let mut store = wasmtime::Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);

        if def.resources.fuel_limit > 0 {
            store
                .set_fuel(def.resources.fuel_limit)
                .context("set wasmtime fuel")?;
        }

        #[cfg(feature = "wasi")]
        let mut linker = wasmtime::Linker::new(&self.engine);

        #[cfg(not(feature = "wasi"))]
        let linker = wasmtime::Linker::new(&self.engine);

        #[cfg(feature = "wasi")]
        wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |state: &mut StoreState| {
            &mut state.wasi
        })
        .context("link wasi preview1")?;

        let instance = linker
            .instantiate(&mut store, &module)
            .context("instantiate wasm module")?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow!("WASM module missing `memory` export"))?;

        let alloc = instance
            .get_typed_func::<u32, u32>(&mut store, "alloc")
            .context("missing `alloc(len: u32) -> u32` export")?;
        let dealloc = instance
            .get_typed_func::<(u32, u32), ()>(&mut store, "dealloc")
            .context("missing `dealloc(ptr: u32, len: u32)` export")?;
        let process = instance
            .get_typed_func::<(u32, u32), u64>(&mut store, "process")
            .context("missing `process(ptr: u32, len: u32) -> u64` export")?;

        let mut input = Box::pin(input);

        let stream = async_stream::try_stream! {
            while let Some(chunk) = input.next().await {
                let chunk = chunk?;
                let in_len: u32 = chunk
                    .len()
                    .try_into()
                    .map_err(|_| anyhow!("input chunk too large"))?;

                let in_ptr = alloc.call(&mut store, in_len).context("alloc input")?;
                memory
                    .write(&mut store, in_ptr as usize, chunk.as_ref())
                    .context("write input to guest memory")?;

                let packed = process
                    .call(&mut store, (in_ptr, in_len))
                    .context("process chunk")?;

                let out_ptr = (packed >> 32) as u32;
                let out_len = (packed & 0xFFFF_FFFF) as u32;

                let mut out = vec![0u8; out_len as usize];
                memory
                    .read(&mut store, out_ptr as usize, &mut out)
                    .context("read output from guest memory")?;

                let _ = dealloc.call(&mut store, (in_ptr, in_len));
                let _ = dealloc.call(&mut store, (out_ptr, out_len));

                yield Bytes::from(out);
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(feature = "wasi")]
fn build_wasi(
    args: &std::collections::HashMap<String, String>,
) -> Result<wasmtime_wasi::preview1::WasiP1Ctx> {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
    for (k, v) in args {
        builder.env(k, v);
    }
    Ok(builder.build_p1())
}

fn build_limits(resources: &ResourceLimits) -> wasmtime::StoreLimits {
    let max_pages = resources.max_memory_pages.max(1) as usize;
    let max_bytes = max_pages.saturating_mul(64 * 1024);
    wasmtime::StoreLimitsBuilder::new()
        .memory_size(max_bytes)
        .build()
}

fn cache_key(image: &str, verification: Option<&ArtifactVerification>) -> String {
    if let Some(v) = verification {
        if !v.sha256.trim().is_empty() {
            return format!("sha256:{}", v.sha256.trim());
        }
    }
    format!("image:{image}")
}

fn verify_artifact(verification: Option<&ArtifactVerification>, bytes: &[u8]) -> Result<()> {
    let Some(verification) = verification else {
        return Ok(());
    };

    let expected = verification.sha256.trim();
    if expected.is_empty() {
        return Ok(());
    }

    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let actual = hex::encode(hasher.finalize());

    if !expected.eq_ignore_ascii_case(&actual) {
        anyhow::bail!("WASM sha256 mismatch (expected {expected}, got {actual})");
    }

    if verification.signature.as_deref().is_some() {
        anyhow::bail!("WASM signature verification not implemented");
    }

    Ok(())
}
