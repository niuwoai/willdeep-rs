use crate::i18n::Language;

pub(super) fn command_candidates(language: Language) -> [(&'static str, &'static str); 12] {
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
            "/mobile",
            language.text(
                "管理手机中继",
                "Manage mobile relay",
                "モバイルリレーを管理",
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
    ]
}
