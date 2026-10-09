use super::*;

#[test]
fn gpu_recovery_advice_keeps_index_options_out_of_query_failures() {
    let document = gpu_recovery_hint(EmbeddingPurpose::Document);
    assert!(document.contains("--index-embedding-concurrency 1"));
    assert!(document.contains(INDEX_CONCURRENCY_ENV));
    let query = gpu_recovery_hint(EmbeddingPurpose::Query);
    assert!(query.contains("--device cpu"));
    assert!(!query.contains(INDEX_CONCURRENCY_ENV));
    assert!(!query.contains("--index-embedding-concurrency"));
}

fn resolve(
    reference: &str,
    requested: Option<usize>,
    index: &str,
    legacy: &str,
) -> (Option<usize>, Vec<String>) {
    let mut warnings = Vec::new();
    let value = resolve_override(
        reference,
        requested,
        |name| {
            Some(
                if name == INDEX_CONCURRENCY_ENV {
                    index
                } else {
                    legacy
                }
                .to_owned(),
            )
        },
        |warning| warnings.push(warning),
    )
    .expect("valid explicit override");
    (value, warnings)
}

#[test]
fn index_override_precedence_and_legacy_scope() {
    let llama = "local/embeddinggemma-300m";
    assert_eq!(resolve(llama, Some(3), "2", "1").0, Some(3));
    assert_eq!(resolve(llama, None, " 02 ", "1").0, Some(2));
    assert_eq!(resolve(llama, None, "", "4").0, Some(4));
    for reference in [
        "local/all-minilm-l6-v2",
        "local/potion-code-16m-v2",
        "qwen/text-embedding-v4",
    ] {
        assert_eq!(resolve(reference, None, "", "4").0, None);
        assert_eq!(resolve(reference, None, "16", "4").0, Some(16));
    }
}

#[test]
fn invalid_environment_warns_and_uses_auto_without_legacy_retry() {
    for value in [
        "0",
        "-1",
        "+2",
        "1.5",
        "1e2",
        "no",
        "999999999999999999999999999",
    ] {
        let (limit, warnings) = resolve("local/embeddinggemma-300m", None, value, "4");
        assert_eq!(limit, None, "{value}");
        assert_eq!(warnings.len(), 1, "{value}");
        assert!(warnings[0].contains(INDEX_CONCURRENCY_ENV));
        assert!(
            resolve("local/embeddinggemma-300m", Some(1), value, "4")
                .1
                .is_empty()
        );
    }
    assert!(resolve_override("local/embeddinggemma-300m", Some(0), |_| None, |_| {}).is_err());
}

#[test]
fn native_limits_cap_at_eight_and_llama_batches_are_serial() {
    let llama = "local/embeddinggemma-300m";
    let transformers = "local/all-minilm-l6-v2";
    assert_eq!(
        local_runtime_limit(llama, Some(24), Some(Device::Cuda)),
        Some(8)
    );
    assert_eq!(local_runtime_limit(llama, None, Some(Device::Cpu)), Some(1));
    assert_eq!(local_runtime_limit(llama, None, Some(Device::Cuda)), None);
    assert_eq!(local_runtime_limit(transformers, Some(24), None), Some(8));
    assert_eq!(batch_concurrency(llama, Some(24)), Some(1));
    assert_eq!(batch_concurrency(transformers, Some(24)), Some(8));
    for reference in ["local/potion-code-16m-v2", "qwen/text-embedding-v4"] {
        assert_eq!(local_runtime_limit(reference, Some(24), None), None);
        assert_eq!(batch_concurrency(reference, Some(24)), Some(24));
    }
}
