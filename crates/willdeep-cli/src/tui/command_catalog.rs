use unicode_width::UnicodeWidthStr;

use crate::i18n::Language;

/// `/help` 的正文：一行一条命令，用法与说明分列对齐。
///
/// 说明文案与补全菜单共用 [`command_candidates`]，两处永远不会各说各话；
/// 用法签名单列在这里，因为补全菜单只显示命令本身。
pub(super) fn help_text(language: Language) -> String {
    let usages = command_usages(language);
    let column = usages
        .iter()
        .map(|(usage, _)| UnicodeWidthStr::width(*usage))
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!(
        "System: {}",
        language.text(
            "命令一览（提示词默认交给 Runtime 执行）",
            "Commands (prompts run on the Runtime by default)",
            "コマンド一覧（プロンプトは既定で Runtime が実行）",
        )
    )];
    for (usage, description) in usages {
        // 中文和日文占两列，补空格必须按显示宽度算，不能按字符数。
        let padding = " ".repeat(column.saturating_sub(UnicodeWidthStr::width(usage)));
        lines.push(format!("  {usage}{padding}  {description}"));
    }
    lines.push(format!(
        "  {}",
        language.text(
            "提示：在提示词里写 $技能名 直接调用技能；Ctrl+B 也能开关状态栏。",
            "Tip: write $skill-name in a prompt to invoke a skill; Ctrl+B also toggles the sidebar.",
            "ヒント：プロンプトに $skill-name と書くとスキルを呼び出せます。Ctrl+B でもサイドバーを切替できます。",
        )
    ));
    lines.join("\n")
}

/// 每条命令的用法签名。占位符随语言走，中文用户不该对着 `<task>` 猜要填什么。
fn command_usages(language: Language) -> [(&'static str, &'static str); 19] {
    let descriptions = command_candidates(language);
    let usages = match language {
        Language::ZhCn => [
            "/help",
            "/goal <文本>|off",
            "/compress",
            "/model [模型名]",
            "/routing",
            "/mobile [show|hide|off]",
            "/webapp [status|start|stop|127.0.0.1:端口]",
            "/sidebar [on|off]",
            "/daemon [status|start|stop|upgrade]",
            "/runtime <任务>",
            "/local <任务>",
            "/session <操作>",
            "/history [关键词]",
            "/workspace list|switch <id>",
            "/agent instruct <id> <文本>",
            "/diff",
            "/skills",
            "/clear",
            "/exit",
        ],
        Language::En => [
            "/help",
            "/goal <text>|off",
            "/compress",
            "/model [model]",
            "/routing",
            "/mobile [show|hide|off]",
            "/webapp [status|start|stop|127.0.0.1:PORT]",
            "/sidebar [on|off]",
            "/daemon [status|start|stop|upgrade]",
            "/runtime <task>",
            "/local <task>",
            "/session <action>",
            "/history [query]",
            "/workspace list|switch <id>",
            "/agent instruct <id> <text>",
            "/diff",
            "/skills",
            "/clear",
            "/exit",
        ],
        Language::Ja => [
            "/help",
            "/goal <テキスト>|off",
            "/compress",
            "/model [モデル名]",
            "/routing",
            "/mobile [show|hide|off]",
            "/webapp [status|start|stop|127.0.0.1:ポート]",
            "/sidebar [on|off]",
            "/daemon [status|start|stop|upgrade]",
            "/runtime <タスク>",
            "/local <タスク>",
            "/session <操作>",
            "/history [検索語]",
            "/workspace list|switch <id>",
            "/agent instruct <id> <テキスト>",
            "/diff",
            "/skills",
            "/clear",
            "/exit",
        ],
    };
    // 用法与说明按同一顺序声明，配错了就是把说明贴到别的命令上。
    debug_assert!(
        usages
            .iter()
            .zip(descriptions.iter())
            .all(|(usage, (command, _))| usage
                .split(' ')
                .next()
                .is_some_and(|head| head == *command)),
        "help usages must stay aligned with command_candidates order"
    );
    std::array::from_fn(|index| (usages[index], descriptions[index].1))
}

pub(super) fn command_candidates(language: Language) -> [(&'static str, &'static str); 19] {
    [
        (
            "/help",
            language.text("查看帮助", "Show help", "ヘルプを表示"),
        ),
        (
            "/goal",
            language.text("设置持续目标", "Set persistent goal", "継続目標を設定"),
        ),
        (
            "/compress",
            language.text(
                "压缩会话上下文",
                "Compress conversation context",
                "会話コンテキストを圧縮",
            ),
        ),
        (
            "/model",
            language.text(
                "列出、筛选或切换当前模型",
                "List, filter, or switch the current model",
                "現在のモデルを一覧・絞り込み・切替",
            ),
        ),
        (
            "/routing",
            language.text(
                "配置 Root、Worker 与 Deep 模型路由",
                "Configure Root, Worker, and Deep model routing",
                "Root、Worker、Deep のモデルルーティングを設定",
            ),
        ),
        (
            "/mobile",
            language.text(
                "管理手机中继",
                "Manage mobile relay",
                "モバイルリレーを管理",
            ),
        ),
        (
            "/webapp",
            language.text(
                "启动、停止或查看本地 Web App",
                "Start, stop, or inspect the local Web App",
                "ローカル Web App の起動・停止・確認",
            ),
        ),
        (
            "/sidebar",
            language.text(
                "显示或隐藏右侧状态栏",
                "Show or hide the status sidebar",
                "状態サイドバーの表示・非表示",
            ),
        ),
        (
            "/daemon",
            language.text(
                "查看或升级执行命令的 Runtime",
                "Inspect or upgrade the Runtime that runs commands",
                "コマンドを実行する Runtime の確認・アップグレード",
            ),
        ),
        (
            "/runtime",
            language.text(
                "提交可分离的 Runtime 任务",
                "Submit a detachable Runtime task",
                "切り離し可能な Runtime タスクを送信",
            ),
        ),
        (
            "/local",
            language.text(
                "仅本轮使用进程内 Harness",
                "Use the in-process Harness for one turn",
                "このターンだけプロセス内 Harness を使用",
            ),
        ),
        (
            "/session",
            language.text(
                "管理、搜索或导出会话",
                "Manage, search, or export sessions",
                "セッションの管理・検索・エクスポート",
            ),
        ),
        (
            "/history",
            language.text(
                "打开最近会话面板并继续",
                "Open the recent Session panel and resume",
                "最近のセッションパネルを開いて再開",
            ),
        ),
        (
            "/workspace",
            language.text(
                "列出或切换 Runtime 工作区",
                "List or switch Runtime Workspaces",
                "Runtime ワークスペースの一覧・切替",
            ),
        ),
        (
            "/agent",
            language.text(
                "查看或控制子 Agent",
                "Inspect or control child Agents",
                "子 Agent の表示・操作",
            ),
        ),
        (
            "/diff",
            language.text(
                "打开 Diff Review Center",
                "Open Diff Review Center",
                "Diff Review Center を開く",
            ),
        ),
        (
            "/skills",
            language.text(
                "查看可用技能",
                "List available skills",
                "利用可能なスキルを表示",
            ),
        ),
        (
            "/clear",
            language.text("清空聊天显示", "Clear chat display", "チャット表示を消去"),
        ),
        (
            "/exit",
            language.text("退出 TUI", "Exit the TUI", "TUI を終了"),
        ),
    ]
}
