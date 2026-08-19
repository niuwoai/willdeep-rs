use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    ZhCn,
    En,
    Ja,
}

impl Language {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "zh" | "zh-cn" | "zh-hans" => Ok(Self::ZhCn),
            "en" | "en-us" | "en-gb" => Ok(Self::En),
            "ja" | "ja-jp" => Ok(Self::Ja),
            _ => bail!("unsupported language '{value}'; expected zh-CN, en, or ja"),
        }
    }

    pub fn text(self, zh: &'static str, en: &'static str, ja: &'static str) -> &'static str {
        match self {
            Self::ZhCn => zh,
            Self::En => en,
            Self::Ja => ja,
        }
    }

    /// `text` 的取值版：文案需要插值时用它，三个候选都已构造完毕，只挑一个。
    pub fn pick<T>(self, zh: T, en: T, ja: T) -> T {
        match self {
            Self::ZhCn => zh,
            Self::En => en,
            Self::Ja => ja,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_languages_and_aliases() {
        assert_eq!(Language::parse(Some("zh_CN")).unwrap(), Language::ZhCn);
        assert_eq!(Language::parse(Some("en-US")).unwrap(), Language::En);
        assert_eq!(Language::parse(Some("ja-JP")).unwrap(), Language::Ja);
        assert!(Language::parse(Some("fr")).is_err());
    }
}
