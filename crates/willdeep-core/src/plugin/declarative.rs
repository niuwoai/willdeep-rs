//! 声明式 UI 文档：插件侧栏与简单页面用的那套 JSON 组件树。
//!
//! 这套东西的价值在于**它不是代码**：不执行脚本、不接受 CSS、不接受任意坐标，
//! 所以宿主可以直接原生渲染一份第三方给的界面而不必开沙箱。代价是必须把限制
//! 卡死——包内 Schema 与 MCP 动态 Resource 走的是同一套校验，因为动态那条路
//! 才是真正会被人喂脏数据的那条。

use std::collections::BTreeSet;

use serde_json::Value;

pub const MAX_COMPONENTS: usize = 1000;
pub const MAX_DEPTH: usize = 20;

#[derive(Clone, Debug, thiserror::Error)]
pub enum DeclarativeError {
    #[error("declarative document is not valid JSON: {0}")]
    Json(String),
    #[error("declarative document must be an object with schemaVersion and components")]
    Shape,
    #[error("unsupported declarative schemaVersion {0}, this build understands 1")]
    SchemaVersion(u64),
    #[error("unknown component kind `{0}`")]
    UnknownKind(String),
    #[error("unknown component field `{0}`")]
    UnknownField(String),
    #[error("component id `{0}` is not valid")]
    InvalidId(String),
    #[error("duplicate component id `{0}`")]
    DuplicateId(String),
    #[error("declarative document has more than {MAX_COMPONENTS} components")]
    TooManyComponents,
    #[error("declarative document nests deeper than {MAX_DEPTH} levels")]
    TooDeep,
    #[error("progress must be between 0 and 1, got {0}")]
    InvalidProgress(f64),
    #[error("component `{id}` references unknown command `{command}`")]
    UnknownCommand { id: String, command: String },
}

/// 与共享 schema 的 `kind` 枚举逐字对应。多一项就是宿主单方面扩了契约。
const KINDS: [&str; 14] = [
    "section",
    "list",
    "row",
    "label",
    "icon",
    "badge",
    "status",
    "progress",
    "button",
    "disclosure",
    "separator",
    "emptyState",
    "loadingState",
    "errorState",
];

const FIELDS: [&str; 11] = [
    "id",
    "kind",
    "titleKey",
    "subtitleKey",
    "systemImage",
    "value",
    "progress",
    "command",
    "contextCommands",
    "arguments",
    "children",
];

/// 一棵校验过的组件树。内容仍是 `serde_json::Value`——宿主要做的是保证它
/// **合法**，而不是把它翻译成一堆 Rust 结构体再翻译回 JSON 发给前端。
#[derive(Clone, Debug)]
pub struct DeclarativeDocument {
    pub components: Vec<Value>,
}

impl DeclarativeDocument {
    /// 解析并校验。`known_commands` 是本插件声明过的命令 ID：引用不存在的命令
    /// 会让用户点下去什么也不发生，属于必须拦在装载期的错误。
    pub fn parse(
        source: &str,
        known_commands: &BTreeSet<String>,
    ) -> Result<Self, DeclarativeError> {
        let value: Value = serde_json::from_str(source)
            .map_err(|error| DeclarativeError::Json(error.to_string()))?;
        let object = value.as_object().ok_or(DeclarativeError::Shape)?;
        match object.get("schemaVersion").and_then(Value::as_u64) {
            Some(1) => {}
            Some(other) => return Err(DeclarativeError::SchemaVersion(other)),
            None => return Err(DeclarativeError::Shape),
        }
        let components = object
            .get("components")
            .and_then(Value::as_array)
            .ok_or(DeclarativeError::Shape)?;

        let mut seen = BTreeSet::new();
        let mut count = 0usize;
        for component in components {
            validate(component, known_commands, &mut seen, &mut count, 1)?;
        }
        Ok(Self {
            components: components.clone(),
        })
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({"schemaVersion": 1, "components": self.components})
    }
}

fn validate(
    value: &Value,
    known_commands: &BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    count: &mut usize,
    depth: usize,
) -> Result<(), DeclarativeError> {
    if depth > MAX_DEPTH {
        return Err(DeclarativeError::TooDeep);
    }
    *count += 1;
    if *count > MAX_COMPONENTS {
        return Err(DeclarativeError::TooManyComponents);
    }
    let object = value.as_object().ok_or(DeclarativeError::Shape)?;
    for key in object.keys() {
        if !FIELDS.contains(&key.as_str()) {
            return Err(DeclarativeError::UnknownField(key.clone()));
        }
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or(DeclarativeError::Shape)?;
    if id.is_empty()
        || id.chars().count() > 160
        || !id
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | '-'))
    {
        return Err(DeclarativeError::InvalidId(id.to_owned()));
    }
    if !seen.insert(id.to_owned()) {
        return Err(DeclarativeError::DuplicateId(id.to_owned()));
    }
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(DeclarativeError::Shape)?;
    if !KINDS.contains(&kind) {
        return Err(DeclarativeError::UnknownKind(kind.to_owned()));
    }
    if let Some(progress) = object.get("progress") {
        let progress = progress.as_f64().ok_or(DeclarativeError::Shape)?;
        if !(0.0..=1.0).contains(&progress) {
            return Err(DeclarativeError::InvalidProgress(progress));
        }
    }
    for key in ["command"] {
        if let Some(command) = object.get(key).and_then(Value::as_str)
            && !known_commands.contains(command)
        {
            return Err(DeclarativeError::UnknownCommand {
                id: id.to_owned(),
                command: command.to_owned(),
            });
        }
    }
    if let Some(commands) = object.get("contextCommands").and_then(Value::as_array) {
        for command in commands {
            let command = command.as_str().ok_or(DeclarativeError::Shape)?;
            if !known_commands.contains(command) {
                return Err(DeclarativeError::UnknownCommand {
                    id: id.to_owned(),
                    command: command.to_owned(),
                });
            }
        }
    }
    if let Some(arguments) = object.get("arguments") {
        let arguments = arguments.as_object().ok_or(DeclarativeError::Shape)?;
        if arguments.values().any(|item| !item.is_string()) {
            return Err(DeclarativeError::Shape);
        }
    }
    for child in object
        .get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        validate(child, known_commands, seen, count, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn parses_a_nested_document() {
        let source = r#"{"schemaVersion":1,"components":[
            {"id":"overview","kind":"section","titleKey":"section.overview","children":[
                {"id":"row","kind":"row","children":[
                    {"id":"label","kind":"label","titleKey":"overview.sessions"},
                    {"id":"value","kind":"badge","value":"12"}]}]}]}"#;
        let document = DeclarativeDocument::parse(source, &commands(&[])).expect("parses");
        assert_eq!(document.components.len(), 1);
    }

    #[test]
    fn rejects_duplicate_ids_and_unknown_kinds() {
        let duplicate = r#"{"schemaVersion":1,"components":[
            {"id":"a","kind":"label"},{"id":"a","kind":"label"}]}"#;
        assert!(matches!(
            DeclarativeDocument::parse(duplicate, &commands(&[])),
            Err(DeclarativeError::DuplicateId(_))
        ));
        let unknown = r#"{"schemaVersion":1,"components":[{"id":"a","kind":"webview"}]}"#;
        assert!(matches!(
            DeclarativeDocument::parse(unknown, &commands(&[])),
            Err(DeclarativeError::UnknownKind(_))
        ));
    }

    #[test]
    fn rejects_styling_smuggled_in_as_extra_fields() {
        // 声明式 UI 的全部价值在于它不是代码。多一个 style 字段就是开了口子。
        let source = r#"{"schemaVersion":1,"components":[
            {"id":"a","kind":"label","style":"position:absolute"}]}"#;
        assert!(matches!(
            DeclarativeDocument::parse(source, &commands(&[])),
            Err(DeclarativeError::UnknownField(_))
        ));
    }

    #[test]
    fn rejects_commands_the_plugin_never_declared() {
        let source = r#"{"schemaVersion":1,"components":[
            {"id":"a","kind":"button","titleKey":"k","command":"other.plugin.command"}]}"#;
        assert!(matches!(
            DeclarativeDocument::parse(source, &commands(&["mine.refresh"])),
            Err(DeclarativeError::UnknownCommand { .. })
        ));
        let ok = r#"{"schemaVersion":1,"components":[
            {"id":"a","kind":"button","titleKey":"k","command":"mine.refresh"}]}"#;
        assert!(DeclarativeDocument::parse(ok, &commands(&["mine.refresh"])).is_ok());
    }

    #[test]
    fn rejects_out_of_range_progress() {
        let source =
            r#"{"schemaVersion":1,"components":[{"id":"a","kind":"progress","progress":1.5}]}"#;
        assert!(matches!(
            DeclarativeDocument::parse(source, &commands(&[])),
            Err(DeclarativeError::InvalidProgress(_))
        ));
    }

    #[test]
    fn rejects_documents_that_nest_too_deep() {
        let mut source = String::from(r#"{"id":"leaf","kind":"label"}"#);
        for index in 0..MAX_DEPTH + 1 {
            source = format!(r#"{{"id":"n{index}","kind":"section","children":[{source}]}}"#);
        }
        let document = format!(r#"{{"schemaVersion":1,"components":[{source}]}}"#);
        assert!(matches!(
            DeclarativeDocument::parse(&document, &commands(&[])),
            Err(DeclarativeError::TooDeep)
        ));
    }

    #[test]
    fn rejects_documents_with_too_many_components() {
        let children: Vec<String> = (0..MAX_COMPONENTS + 1)
            .map(|index| format!(r#"{{"id":"c{index}","kind":"label"}}"#))
            .collect();
        let document = format!(
            r#"{{"schemaVersion":1,"components":[{}]}}"#,
            children.join(",")
        );
        assert!(matches!(
            DeclarativeDocument::parse(&document, &commands(&[])),
            Err(DeclarativeError::TooManyComponents)
        ));
    }
}
