//! 下一句预测的实弹评测：真 Provider、固定样本集，量出预测到底靠不靠谱。
//!
//! rc29 的预测只有单元测试盯着清洗规则；提示词是英文硬编码的，输出语言靠
//! 「跟用户走」这一条规则，没人在真模型上验过。这里逐样本发一次真实请求，
//! 记下**清洗前**的原始输出、清洗后的结果、耗时与 token，交给
//! `scripts/input_suggestion_eval.rb` 归档与算指标。
//!
//! 不是 CI 的一部分：默认 `#[ignore]`，需要真实凭据、每跑一轮都花钱。
#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::input_suggestion::{payload, request_messages, sanitize};
use crate::provider::{ApiDialect, Provider, ProviderConfig, ProviderKind, build_provider};
use crate::types::Message;

/// 样本文件的形状，见 `bench/input-suggestion/README.md`。
#[derive(serde::Deserialize)]
struct Sample {
    id: String,
    language: String,
    /// `suggest` / `none` / `reject`。
    expect: String,
    messages: Vec<SampleMessage>,
}

#[derive(serde::Deserialize)]
struct SampleMessage {
    role: String,
    content: String,
}

#[derive(serde::Serialize)]
struct SampleResult {
    id: String,
    language: String,
    expect: String,
    raw: Option<String>,
    cleaned: Option<String>,
    error: Option<String>,
    elapsed_ms: u128,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn load_samples(dir: &std::path::Path) -> Vec<Sample> {
    let mut paths = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read samples {}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
        })
        .collect()
}

fn conversation(sample: &Sample) -> Vec<Message> {
    sample
        .messages
        .iter()
        .map(|message| match message.role.as_str() {
            "user" => Message::user(message.content.clone()),
            "assistant" => Message::assistant(message.content.clone(), Vec::new()),
            other => panic!("sample {}: unknown role {other}", sample.id),
        })
        .collect()
}

fn samples_dir() -> PathBuf {
    env_value("WILLDEEP_SUGGEST_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/input-suggestion/samples")
        })
}

/// 样本集本身的自检：不联网，常规测试里跑。样本写错了（缺一边、`expect`
/// 拼错、`reject` 样本没埋假 key），付费那一轮测的就不是模型。
#[test]
fn every_sample_is_well_formed() {
    let samples = load_samples(&samples_dir());
    assert!(samples.len() >= 15, "样本不足 15 条：{}", samples.len());
    let mut ids = std::collections::BTreeSet::new();
    for sample in &samples {
        assert!(
            ids.insert(sample.id.clone()),
            "重复的样本 id：{}",
            sample.id
        );
        assert!(
            ["suggest", "none", "reject"].contains(&sample.expect.as_str()),
            "{}: expect 只能是 suggest / none / reject",
            sample.id
        );
        assert!(
            ["zh", "en", "ja"].contains(&sample.language.as_str()),
            "{}: language 只能是 zh / en / ja",
            sample.id
        );
        assert_eq!(
            sample.messages.last().map(|message| message.role.as_str()),
            Some("assistant"),
            "{}: 最后一条必须是助手",
            sample.id
        );
        assert!(
            payload(&conversation(sample)).is_some(),
            "{}: 组不出预测正文",
            sample.id
        );
        if sample.expect == "reject" {
            assert!(
                sample
                    .messages
                    .iter()
                    .any(|message| message.content.contains("sk-test-")),
                "{}: reject 样本必须埋一个 sk-test- 假 key",
                sample.id
            );
        }
    }
    for language in ["zh", "en", "ja"] {
        assert!(
            samples.iter().any(|sample| sample.language == language),
            "缺 {language} 样本"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live fire: needs real provider credentials and spends money; run scripts/input_suggestion_eval.rb"]
async fn input_suggestion_live_fire() {
    let (Some(base_url), Some(api_key)) = (
        env_value("WILLDEEP_RANGE_API_BASE"),
        env_value("WILLDEEP_RANGE_API_KEY"),
    ) else {
        panic!(
            "set WILLDEEP_RANGE_API_BASE and WILLDEEP_RANGE_API_KEY (scripts/input_suggestion_eval.rb reads them from ~/.willdeep/config.toml)"
        );
    };
    let model = env_value("WILLDEEP_RANGE_MODEL").unwrap_or_else(|| "glm-5".to_owned());
    let out = env_value("WILLDEEP_SUGGEST_OUT")
        .map(PathBuf::from)
        .expect("set WILLDEEP_SUGGEST_OUT");

    // 与宿主装配的标题 / 预测 Provider 同一个方言与输出上限口径：一句话用不了多少。
    let mut config = ProviderConfig::new(
        ProviderKind::infer(&base_url),
        ApiDialect::ChatCompletions,
        base_url,
        api_key,
        model.clone(),
    );
    config.max_output_tokens = 1_024;
    let provider: Arc<dyn Provider> = build_provider(config).expect("build provider");

    let mut results = Vec::new();
    for sample in load_samples(&samples_dir()) {
        let body = payload(&conversation(&sample)).expect("well-formed sample");
        let started = Instant::now();
        let outcome = provider.complete(&request_messages(&body), &[]).await;
        let elapsed_ms = started.elapsed().as_millis();
        let result = match outcome {
            Ok(completion) => SampleResult {
                cleaned: sanitize(&completion.content),
                raw: Some(completion.content),
                error: None,
                elapsed_ms,
                input_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.input_tokens),
                output_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.output_tokens),
                id: sample.id,
                language: sample.language,
                expect: sample.expect,
            },
            Err(error) => SampleResult {
                raw: None,
                cleaned: None,
                error: Some(error.to_string()),
                elapsed_ms,
                input_tokens: None,
                output_tokens: None,
                id: sample.id,
                language: sample.language,
                expect: sample.expect,
            },
        };
        println!(
            "{} | {} | raw={:?} | cleaned={:?} | {}ms | in={:?} out={:?}{}",
            result.id,
            model,
            result.raw,
            result.cleaned,
            result.elapsed_ms,
            result.input_tokens,
            result.output_tokens,
            result
                .error
                .as_deref()
                .map(|error| format!(" | error={error}"))
                .unwrap_or_default()
        );
        results.push(result);
    }

    let report = serde_json::json!({ "model": model, "cases": results });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("report directory");
    }
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&report).expect("encode report"),
    )
    .expect("write report");
}
