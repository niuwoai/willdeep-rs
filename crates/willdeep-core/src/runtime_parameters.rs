//! Cross-client execution contract. No credentials, paths, prompts or model text.
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA: &str = "willdeep.runtime-parameters.v1";
pub const FILE_NAME: &str = "runtime-parameters.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeParameters {
    pub schema: String,
    pub max_turns: usize,
    #[serde(deserialize_with = "required_nullable")]
    pub token_budget: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub goal_token_budget: Option<u64>,
    pub goal_wall_clock_minutes: u64,
    pub goal_max_continuations: usize,
    pub input_suggestions: bool,
    pub small_model_routing: bool,
    pub auto_dispatch_read_only: bool,
    pub max_deep_calls_per_harness: usize,
}

fn required_nullable<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(decoder)
}

impl Default for RuntimeParameters {
    fn default() -> Self {
        Self {
            schema: SCHEMA.into(),
            max_turns: 200,
            token_budget: None,
            goal_token_budget: None,
            goal_wall_clock_minutes: 240,
            goal_max_continuations: 64,
            input_suggestions: true,
            small_model_routing: true,
            auto_dispatch_read_only: false,
            max_deep_calls_per_harness: 1,
        }
    }
}

impl RuntimeParameters {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err("unsupported runtime parameter schema".into());
        }
        for (name, value, minimum, maximum) in [
            ("max_turns", self.max_turns as u64, 1, 1000),
            (
                "goal_wall_clock_minutes",
                self.goal_wall_clock_minutes,
                1,
                10080,
            ),
            (
                "goal_max_continuations",
                self.goal_max_continuations as u64,
                1,
                10000,
            ),
            (
                "max_deep_calls_per_harness",
                self.max_deep_calls_per_harness as u64,
                0,
                16,
            ),
        ] {
            if !(minimum..=maximum).contains(&value) {
                return Err(format!("{name} must be between {minimum} and {maximum}"));
            }
        }
        for (name, value) in [
            ("token_budget", self.token_budget),
            ("goal_token_budget", self.goal_token_budget),
        ] {
            if value.is_some_and(|value| !(1000..=10_000_000).contains(&value)) {
                return Err(format!("{name} must be null or between 1000 and 10000000"));
            }
        }
        Ok(())
    }

    /// Missing file means legacy configuration; unreadable/invalid files fail closed.
    pub fn load(home: &Path) -> Result<Option<Self>, String> {
        let data = match std::fs::read(home.join(FILE_NAME)) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("cannot read runtime-parameters.json".into()),
        };
        if data.len() > 65536 {
            return Err("runtime-parameters.json exceeds 65536 bytes".into());
        }
        let value: Self = serde_json::from_slice(&data)
            .map_err(|_| "invalid runtime-parameters.json structure".to_owned())?;
        value.validate()?;
        Ok(Some(value))
    }

    /// A fixed scalar ordering avoids JSON encoder ordering differences across clients.
    pub fn canonical(&self) -> String {
        let nullable =
            |value: Option<u64>| value.map_or_else(|| "null".into(), |value| value.to_string());
        format!(
            "{{\"schema\":\"{}\",\"max_turns\":{},\"token_budget\":{},\"goal_token_budget\":{},\"goal_wall_clock_minutes\":{},\"goal_max_continuations\":{},\"input_suggestions\":{},\"small_model_routing\":{},\"auto_dispatch_read_only\":{},\"max_deep_calls_per_harness\":{}}}",
            SCHEMA,
            self.max_turns,
            nullable(self.token_budget),
            nullable(self.goal_token_budget),
            self.goal_wall_clock_minutes,
            self.goal_max_continuations,
            self.input_suggestions,
            self.small_model_routing,
            self.auto_dispatch_read_only,
            self.max_deep_calls_per_harness
        )
    }

    pub fn fingerprint(&self) -> String {
        format!("{:x}", Sha256::digest(self.canonical().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_boundaries_and_rejects_unknown_fields() {
        let mut value = RuntimeParameters::default();
        assert!(value.validate().is_ok());
        value.token_budget = Some(0);
        assert!(value.validate().is_err());
        value.token_budget = Some(1000);
        value.max_turns = 1001;
        assert!(value.validate().is_err());
        let json = RuntimeParameters::default().canonical().replace(
            "\"max_turns\":200",
            "\"max_turns\":200,\"api_key\":\"forbidden\"",
        );
        assert!(serde_json::from_str::<RuntimeParameters>(&json).is_err());
    }
    #[test]
    fn canonical_roundtrip_has_stable_fingerprint() {
        let value = RuntimeParameters::default();
        let decoded: RuntimeParameters = serde_json::from_str(&value.canonical()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.fingerprint(), value.fingerprint());
        assert_eq!(value.fingerprint().len(), 64);
        assert_eq!(
            value.fingerprint(),
            "cbbe63a9094a7d8546d52f38775bf9e8e0f5e010cfabc767a9d4ffb42ee00e1b"
        );
        assert!(
            serde_json::from_str::<RuntimeParameters>(
                &value.canonical().replace("\"token_budget\":null,", "")
            )
            .is_err()
        );
    }
}
