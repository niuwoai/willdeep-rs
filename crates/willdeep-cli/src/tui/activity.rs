use super::*;

#[derive(Default)]
pub(super) struct ToolActivity {
    pub(super) requested: usize,
    pub(super) completed: usize,
    pub(super) failed: usize,
    pub(super) counts: BTreeMap<String, usize>,
    pub(super) details: Vec<String>,
}

impl ToolActivity {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn requested(&mut self, name: &str) {
        self.requested += 1;
        *self.counts.entry(name.to_owned()).or_default() += 1;
        self.details.push(format!("… {name}"));
    }

    pub(super) fn completed(&mut self, name: &str, is_error: bool) {
        self.completed += 1;
        self.failed += usize::from(is_error);
        let pending = format!("… {name}");
        if let Some(value) = self
            .details
            .iter_mut()
            .rev()
            .find(|value| **value == pending)
        {
            *value = format!("{} {name}", if is_error { "✗" } else { "✓" });
        }
    }

    pub(super) fn summary(&self, language: Language) -> String {
        let calls = self
            .counts
            .iter()
            .map(|(name, count)| format!("{name}×{count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let progress = if self.completed < self.requested {
            format!(
                "{}/{} {}",
                self.completed,
                self.requested,
                language.text("完成", "complete", "完了")
            )
        } else {
            format!(
                "{} {}",
                self.requested,
                language.text("次调用", "calls", "回呼び出し")
            )
        };
        let failed = if self.failed > 0 {
            format!(
                " · {} {}",
                self.failed,
                language.text("失败", "failed", "失敗")
            )
        } else {
            String::new()
        };
        format!(
            "{}: {progress}{failed} · {calls}",
            language.text("工具", "Tools", "ツール")
        )
    }
}
