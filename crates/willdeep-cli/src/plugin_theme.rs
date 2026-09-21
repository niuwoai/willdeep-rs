//! 插件页面的宿主配色：注入 `:root { --willdeep-* }`，让同一个插件包在
//! macOS 宿主（Xedit `AgentPluginPageHost.composedHTML`）与 Web 宿主上都像
//! 原生界面。变量名与那边一一对应，值取自 `web/src/theme.css` 的对应层次，
//! 所以插件页面贴在 Web 宿主里时与周围界面是同一套颜色。
//!
//! 和 macOS 宿主的差别在于**切换不重载**：macOS 那边每次换配色重新合成整页；
//! 这里两套配色一次注入，由 `<html data-willdeep-color-scheme>` 选一套，桥在
//! 收到新的 context 时改这个属性。插件页面里的表单、滚动位置不会因为用户
//! 切了一下主题就丢掉。
//!
//! 初始值来自 iframe 地址上的 `?colorScheme=`：父页面挂 iframe 时就知道当前
//! 解析出来的配色，带上它可以避免「先按系统画一帧、context 到了再翻色」。
//! 没带（或带了认不出的值）时跟随 `prefers-color-scheme`——iframe 里的这条
//! 媒体查询看的是系统设置，与宿主「跟随系统」那档一致。

/// 宿主告诉插件页面的配色。只有这两个值：「跟随系统」在父页面那里就已经
/// 解析成其中之一，插件拿到的永远是一个确定的答案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageColorScheme {
    Light,
    Dark,
}

impl PageColorScheme {
    /// 认不出的值当作没给：地址栏上的东西谁都能改，拿它当真会把一个
    /// 任意字符串写进页面脚本里。
    pub(crate) fn parse(value: Option<&str>) -> Option<Self> {
        match value? {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// 页面正文字号。Web 宿主没有 macOS 那个可调的聊天字号，取界面正文的 14px。
const BODY_FONT_SIZE_PX: u32 = 14;

/// 标记属性，桥脚本（`plugin_bridge.js`）改的也是它。
const SCHEME_ATTRIBUTE: &str = "data-willdeep-color-scheme";

/// 一套配色落成的变量声明。
struct Palette {
    background: &'static str,
    foreground: &'static str,
    secondary: &'static str,
    accent: &'static str,
    scheme: &'static str,
}

/// 对应 `theme.css` 深色的 `--bg-page` / `--text` / `--text-dim` / `--accent`。
const DARK: Palette = Palette {
    background: "#080d12",
    foreground: "#e7edf4",
    secondary: "#8b99aa",
    accent: "#78a9ff",
    scheme: "dark",
};

/// 对应 `theme.css` 浅色的同名层次。
const LIGHT: Palette = Palette {
    background: "#f4f7fb",
    foreground: "#1b2632",
    secondary: "#5d6b7c",
    accent: "#1f6feb",
    scheme: "light",
};

fn declarations(palette: &Palette) -> String {
    format!(
        "--willdeep-bg: {}; --willdeep-fg: {}; --willdeep-secondary: {}; \
         --willdeep-accent: {}; --willdeep-body-font-size: {BODY_FONT_SIZE_PX}px; \
         color-scheme: {};",
        palette.background, palette.foreground, palette.secondary, palette.accent, palette.scheme
    )
}

/// 注入到 `<head>` 最前面的样式（和可选的初始配色脚本）。
///
/// 基础规则与 macOS 宿主那段 `<style>` 同一份：插件在那边没写背景色时拿到的是
/// 宿主背景，这边也得一样，否则同一个插件包两端长得不一样。这些规则只用元素
/// 选择器，插件自己的样式写在后面、特异性相同，照样盖得住。
pub(crate) fn theme_head(initial: Option<PageColorScheme>) -> String {
    let dark = declarations(&DARK);
    let light = declarations(&LIGHT);
    let style = format!(
        "<style id=\"willdeep-host-theme\">\n\
         :root {{ {dark} }}\n\
         @media (prefers-color-scheme: light) {{ :root:not([{SCHEME_ATTRIBUTE}]) {{ {light} }} }}\n\
         :root[{SCHEME_ATTRIBUTE}=\"dark\"] {{ {dark} }}\n\
         :root[{SCHEME_ATTRIBUTE}=\"light\"] {{ {light} }}\n\
         html, body {{ margin: 0; min-height: 100%; background: var(--willdeep-bg); \
         color: var(--willdeep-fg); font-family: Inter, ui-sans-serif, system-ui, -apple-system, sans-serif; \
         font-size: var(--willdeep-body-font-size); }}\n\
         input, button, select, textarea {{ accent-color: var(--willdeep-accent); }}\n\
         a {{ color: var(--willdeep-accent); }}\n\
         </style>"
    );
    match initial {
        // 值来自上面的白名单枚举，不是请求原文，拼进脚本是安全的。
        Some(scheme) => format!(
            "{style}\n<script>document.documentElement.setAttribute('{SCHEME_ATTRIBUTE}', '{}');</script>",
            scheme.as_str()
        ),
        None => style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_known_schemes_are_accepted() {
        let cases = [
            (Some("light"), Some(PageColorScheme::Light)),
            (Some("dark"), Some(PageColorScheme::Dark)),
            (Some("system"), None),
            (Some("LIGHT"), None),
            (Some("light');alert(1);//"), None),
            (Some(""), None),
            (None, None),
        ];
        for (input, expected) in cases {
            assert_eq!(PageColorScheme::parse(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn both_palettes_carry_every_variable_the_macos_host_injects() {
        let head = theme_head(None);
        for variable in [
            "--willdeep-bg:",
            "--willdeep-fg:",
            "--willdeep-secondary:",
            "--willdeep-accent:",
            "--willdeep-body-font-size: 14px",
            "color-scheme: dark",
            "color-scheme: light",
        ] {
            assert!(head.contains(variable), "missing {variable}");
        }
        assert!(head.contains(DARK.background) && head.contains(LIGHT.background));
    }

    #[test]
    fn a_known_initial_scheme_is_pinned_before_the_page_paints() {
        let head = theme_head(Some(PageColorScheme::Light));
        assert!(head.contains("setAttribute('data-willdeep-color-scheme', 'light')"));
        let head = theme_head(Some(PageColorScheme::Dark));
        assert!(head.contains("setAttribute('data-willdeep-color-scheme', 'dark')"));
    }

    #[test]
    fn without_an_initial_scheme_the_page_follows_the_system() {
        let head = theme_head(None);
        assert!(!head.contains("<script>"));
        assert!(head.contains("@media (prefers-color-scheme: light)"));
    }
}
