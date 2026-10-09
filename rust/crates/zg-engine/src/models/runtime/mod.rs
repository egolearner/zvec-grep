//! Process-level model ownership, caching, and compute resources.

mod compute;
mod concurrency;
mod manager;

pub(super) use compute::ModelComputeRuntime;
pub(super) use concurrency::{LOCAL_CONCURRENCY_CAP, gpu_recovery_hint};
pub(crate) use concurrency::{batch_concurrency, resolve_index_concurrency};
pub(crate) use manager::{ModelRuntimeLease, ModelRuntimeManager, ModelRuntimeRequest};
