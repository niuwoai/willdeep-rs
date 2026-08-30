export const languages = ["zh-CN", "en", "ja"] as const;
export type Language = (typeof languages)[number];

const zhCN = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "工作区", session: "历史会话", newSession: "新会话", language: "语言",
  version: "版本", expand: "展开", collapse: "收起", searchSkills: "搜索技能",
  modelRouting: "模型与路由", routingLoading: "正在读取配置…", routingLoadFailed: "读取模型路由失败", routingSaveFailed: "保存模型路由失败", routingStale: "配置文件已被其他窗口或手工修改，请重新加载", routingApplyHint: "保存后对新 Harness 和子 Agent 生效；正在运行的任务保持不变。", routingOverrideHint: "当前命令行 Provider 覆盖", routingRootProvider: "Root Provider", routingRootModel: "Root 模型", routingDeepBudget: "Deep 调用预算", routingEnabled: "启用小模型优先路由", routingAutoDispatch: "自动派发高置信度只读任务", routingProfile: "工种", routingRecommended: "推荐默认", routingProvider: "Provider", routingModel: "模型覆盖", routingContext: "上下文窗口", routingEffective: "当前生效", routingInheritRoot: "继承 Root", routingProviderDefault: "Provider 默认模型", routingSave: "保存设置", routingSaving: "正在保存…", routingSaved: "已保存", routingProfileScout: "侦查", routingProfileReader: "阅读", routingProfileEditor: "单文件编辑", routingProfileImplementer: "实现", routingProfileTester: "测试与审核", routingProfileOpsRunner: "运维执行", routingProfileJudge: "独立裁判", routingProfileTestFixer: "测试修复", routingProfileBuildFixer: "构建修复", routingProfileLogInspector: "日志分析", routingProfileGitDetective: "Git 追溯", routingProfileDeep: "Deep",
  activeWorkspace: "当前", readOnly: "只读", smartApproval: "智能审核", workspaceWrite: "工作区可写",
  runtimeActivity: "Runtime 活动", tools: "工具", running: "运行中", artifacts: "产物", agents: "Agent", agent: "Agent", tasks: "任务", task: "后台任务", needsAttention: "需要处理", waitingApproval: "等待审批", waitingAnswer: "等待回答", blocked: "已阻塞", queued: "等待执行", cancelling: "正在取消", cancelled: "已取消", idle: "空闲", turn: "轮次", unknownValue: "—", tokenUnit: "Token", secondsUnit: "秒", worktree: "工作树",
  allowOnce: "允许一次", deny: "拒绝", alwaysAllow: "始终允许", submitAnswer: "提交", otherAnswer: "其他回答", instruct: "补充指令", retry: "重试", changeModel: "换模型重试", agentPrompt: "输入要补充给 Agent 的指令", agentModelPrompt: "输入重试要使用的模型", runtimeActionFailed: "Runtime 操作失败",
  newReadOnlyAgent: "新建只读子 Agent", agentProfile: "Agent 类型", agentProfileScout: "侦查", agentProfileReader: "阅读", agentProfileJudge: "独立裁判", agentProfileLogInspector: "日志分析", agentProfileGitDetective: "Git 追溯", agentTask: "Agent 任务", agentTaskPlaceholder: "描述独立调查任务", spawnAgent: "创建", activeSessionRequired: "请先发送一个任务；父会话运行期间可创建子 Agent。",
  details: "详情", agentDetails: "Agent 详情", taskDetails: "任务详情", closeDetails: "关闭详情", interrupted: "已中断", duration: "耗时", runningFor: "已运行", finishedHidden: "已结束", minutesUnit: "分", hoursUnit: "小时", exitCode: "退出码", failureDomain: "失败类别", failureProvider: "Provider", failurePolicy: "权限策略", failureTool: "工具", failureHarness: "Harness", failureInternal: "内部", noStructuredActivity: "暂无结构化活动", structuredToolLog: "工具时间线", diffArtifacts: "Diff 产物", changedItems: "项变更", currentTool: "当前工具", structuredLogPrivacy: "仅显示脱敏后的结构化状态，不包含 Prompt、工具参数、输出或内部错误。",
  languageName: "简体中文", welcomeTitle: "准备好了，从哪里开始？",
  welcomeBody: "选择一个工作区，直接描述你想实现、修复或调查的事情。",
  promptPlaceholder: "描述你想完成的任务…", send: "发送", working: "正在处理",
  loadFailed: "加载失败", requestFailed: "请求失败", noSessions: "这个工作区还没有会话",
  emptyReply: "Harness 没有返回文本",
  stop: "停止生成", sendHint: "Enter 发送 · Shift+Enter 换行", commands: "命令",
  thinking: "正在思考", reconnecting: "正在恢复运行中的会话", streamDisconnected: "事件流已断开", stopping: "正在停止", stopped: "已停止", toolRunning: "运行中", toolDone: "已完成", toolFailed: "失败",
  attachments: "附件", removeAttachment: "删除附件", pastedText: "粘贴文本", pastedImage: "粘贴图片", attachmentPrompt: "请分析附件", attachmentCount: "个附件",
  helpText: "可用命令：/help、/goal <目标>、/goal off、/compress、/skills、/clear。输入 $ 可选择技能。", cleared: "聊天显示已清空", goalSet: "目标模式已开启", goalOff: "目标模式已关闭",
  skills: "技能", noSkills: "当前工作区没有发现技能",
  searchSessions: "搜索会话", renameSession: "重命名", forkSession: "分叉", archiveSession: "归档", unarchiveSession: "取消归档", deleteSession: "删除", exportSession: "导出",
  pinSession: "置顶", unpinSession: "取消置顶", pinned: "已置顶",
  deleteDialogTitle: "删除会话", cancel: "取消", confirmDelete: "删除",
  renamePrompt: "输入新的会话名称", forkPrompt: "输入分叉会话名称", deleteConfirm: "永久删除这个会话及其本地历史？此操作无法撤销。", archived: "已归档", sessionActionFailed: "会话操作失败",
};

export type Messages = typeof zhCN;

const en: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "Workspace", session: "Sessions", newSession: "New session", language: "Language",
  version: "Version", expand: "Expand", collapse: "Collapse", searchSkills: "Search skills",
  modelRouting: "Models & routing", routingLoading: "Loading configuration…", routingLoadFailed: "Failed to load model routing", routingSaveFailed: "Failed to save model routing", routingStale: "The configuration changed in another window or on disk; reload it first", routingApplyHint: "Saved values apply to new harnesses and child Agents; running work is unchanged.", routingOverrideHint: "Active command-line provider override", routingRootProvider: "Root provider", routingRootModel: "Root model", routingDeepBudget: "Deep call budget", routingEnabled: "Enable small-model-first routing", routingAutoDispatch: "Auto-dispatch high-confidence read-only work", routingProfile: "Profile", routingRecommended: "Recommended", routingProvider: "Provider", routingModel: "Model override", routingContext: "Context window", routingEffective: "Effective", routingInheritRoot: "Inherit Root", routingProviderDefault: "Provider default model", routingSave: "Save settings", routingSaving: "Saving…", routingSaved: "Saved", routingProfileScout: "Scout", routingProfileReader: "Reader", routingProfileEditor: "Single-file editor", routingProfileImplementer: "Implementer", routingProfileTester: "Tester", routingProfileOpsRunner: "Ops runner", routingProfileJudge: "Judge", routingProfileTestFixer: "Test fixer", routingProfileBuildFixer: "Build fixer", routingProfileLogInspector: "Log inspector", routingProfileGitDetective: "Git detective", routingProfileDeep: "Deep",
  activeWorkspace: "Active", readOnly: "Read only", smartApproval: "Smart approval", workspaceWrite: "Workspace write",
  runtimeActivity: "Runtime activity", tools: "Tools", running: "Running", artifacts: "Artifacts", agents: "Agents", agent: "Agent", tasks: "Tasks", task: "Background task", needsAttention: "Needs attention", waitingApproval: "Waiting for approval", waitingAnswer: "Waiting for answer", blocked: "Blocked", queued: "Queued", cancelling: "Cancelling", cancelled: "Cancelled", idle: "Idle", turn: "Turn", unknownValue: "—", tokenUnit: "tokens", secondsUnit: "s", worktree: "worktree",
  allowOnce: "Allow once", deny: "Deny", alwaysAllow: "Always allow", submitAnswer: "Submit", otherAnswer: "Other answer", instruct: "Instruct", retry: "Retry", changeModel: "Retry with model", agentPrompt: "Enter an instruction for the Agent", agentModelPrompt: "Enter the model to use for the retry", runtimeActionFailed: "Runtime action failed",
  newReadOnlyAgent: "New read-only child Agent", agentProfile: "Agent profile", agentProfileScout: "Scout", agentProfileReader: "Reader", agentProfileJudge: "Judge", agentProfileLogInspector: "Log inspector", agentProfileGitDetective: "Git detective", agentTask: "Agent task", agentTaskPlaceholder: "Describe an independent investigation", spawnAgent: "Create", activeSessionRequired: "Send a task first; child Agents can be created while the parent session is active.",
  details: "Details", agentDetails: "Agent details", taskDetails: "Task details", closeDetails: "Close details", interrupted: "Interrupted", duration: "Duration", runningFor: "Running for", finishedHidden: "finished", minutesUnit: "m", hoursUnit: "h", exitCode: "Exit code", failureDomain: "Failure domain", failureProvider: "Provider", failurePolicy: "Policy", failureTool: "Tool", failureHarness: "Harness", failureInternal: "Internal", noStructuredActivity: "No structured activity yet", structuredToolLog: "Tool timeline", diffArtifacts: "Diff artifacts", changedItems: "changes", currentTool: "Current tool", structuredLogPrivacy: "Shows redacted structured status only; prompts, tool arguments, outputs, and internal errors are not included.",
  languageName: "English", welcomeTitle: "Ready. Where should we start?",
  welcomeBody: "Choose a workspace and describe what you want to build, fix, or investigate.",
  promptPlaceholder: "Describe the task you want to complete…", send: "Send", working: "Working",
  loadFailed: "Failed to load", requestFailed: "Request failed", noSessions: "No sessions in this workspace",
  emptyReply: "The harness returned no text",
  stop: "Stop generating", sendHint: "Enter to send · Shift+Enter for newline", commands: "Commands",
  thinking: "Thinking", reconnecting: "Reconnecting to the active session", streamDisconnected: "The event stream disconnected", stopping: "Stopping", stopped: "Stopped", toolRunning: "Running", toolDone: "Done", toolFailed: "Failed",
  attachments: "Attachments", removeAttachment: "Remove attachment", pastedText: "Pasted text", pastedImage: "Pasted image", attachmentPrompt: "Please analyze the attachments", attachmentCount: "attachments",
  helpText: "Commands: /help, /goal <text>, /goal off, /compress, /skills, /clear. Type $ to choose a skill.", cleared: "Chat display cleared", goalSet: "Goal mode enabled", goalOff: "Goal mode disabled",
  skills: "Skills", noSkills: "No skills found in this workspace",
  searchSessions: "Search sessions", renameSession: "Rename", forkSession: "Fork", archiveSession: "Archive", unarchiveSession: "Unarchive", deleteSession: "Delete", exportSession: "Export",
  pinSession: "Pin", unpinSession: "Unpin", pinned: "Pinned",
  deleteDialogTitle: "Delete session", cancel: "Cancel", confirmDelete: "Delete",
  renamePrompt: "Enter a new session title", forkPrompt: "Enter a title for the fork", deleteConfirm: "Permanently delete this session and its local history? This cannot be undone.", archived: "Archived", sessionActionFailed: "Session action failed",
};

const ja: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "ワークスペース", session: "履歴", newSession: "新しいセッション", language: "言語",
  version: "バージョン", expand: "展開", collapse: "折りたたむ", searchSkills: "スキルを検索",
  modelRouting: "モデルとルーティング", routingLoading: "設定を読み込み中…", routingLoadFailed: "モデルルーティングの読込に失敗", routingSaveFailed: "モデルルーティングの保存に失敗", routingStale: "設定ファイルが別の画面または手動操作で変更されました。再読込してください", routingApplyHint: "保存内容は新しい Harness と子 Agent に適用され、実行中の処理は変わりません。", routingOverrideHint: "現在のコマンドライン Provider 上書き", routingRootProvider: "Root Provider", routingRootModel: "Root モデル", routingDeepBudget: "Deep 呼出予算", routingEnabled: "小モデル優先ルーティングを有効化", routingAutoDispatch: "高信頼の読取タスクを自動委任", routingProfile: "プロファイル", routingRecommended: "推奨既定", routingProvider: "Provider", routingModel: "モデル上書き", routingContext: "コンテキスト窓", routingEffective: "現在有効", routingInheritRoot: "Root を継承", routingProviderDefault: "Provider の既定モデル", routingSave: "設定を保存", routingSaving: "保存中…", routingSaved: "保存しました", routingProfileScout: "偵察", routingProfileReader: "読解", routingProfileEditor: "単一ファイル編集", routingProfileImplementer: "実装", routingProfileTester: "テスト・レビュー", routingProfileOpsRunner: "運用実行", routingProfileJudge: "独立判定", routingProfileTestFixer: "テスト修正", routingProfileBuildFixer: "ビルド修正", routingProfileLogInspector: "ログ解析", routingProfileGitDetective: "Git 追跡", routingProfileDeep: "Deep",
  activeWorkspace: "現在", readOnly: "読み取り専用", smartApproval: "スマート承認", workspaceWrite: "ワークスペース書込",
  runtimeActivity: "Runtime アクティビティ", tools: "ツール", running: "実行中", artifacts: "成果物", agents: "Agent", agent: "Agent", tasks: "タスク", task: "バックグラウンドタスク", needsAttention: "要対応", waitingApproval: "承認待ち", waitingAnswer: "回答待ち", blocked: "ブロック中", queued: "待機中", cancelling: "キャンセル中", cancelled: "キャンセル済み", idle: "待機", turn: "ターン", unknownValue: "—", tokenUnit: "トークン", secondsUnit: "秒", worktree: "Worktree",
  allowOnce: "一度許可", deny: "拒否", alwaysAllow: "常に許可", submitAnswer: "送信", otherAnswer: "その他の回答", instruct: "指示を追加", retry: "再試行", changeModel: "モデルを変更して再試行", agentPrompt: "Agent への追加指示を入力", agentModelPrompt: "再試行に使用するモデルを入力", runtimeActionFailed: "Runtime 操作に失敗しました",
  newReadOnlyAgent: "読み取り専用子 Agent を作成", agentProfile: "Agent プロファイル", agentProfileScout: "偵察", agentProfileReader: "読解", agentProfileJudge: "独立判定", agentProfileLogInspector: "ログ解析", agentProfileGitDetective: "Git 追跡", agentTask: "Agent タスク", agentTaskPlaceholder: "独立した調査タスクを入力", spawnAgent: "作成", activeSessionRequired: "先にタスクを送信してください。親セッションの実行中に子 Agent を作成できます。",
  details: "詳細", agentDetails: "Agent 詳細", taskDetails: "タスク詳細", closeDetails: "詳細を閉じる", interrupted: "中断", duration: "所要時間", runningFor: "実行時間", finishedHidden: "終了済み", minutesUnit: "分", hoursUnit: "時間", exitCode: "終了コード", failureDomain: "失敗区分", failureProvider: "Provider", failurePolicy: "ポリシー", failureTool: "ツール", failureHarness: "Harness", failureInternal: "内部", noStructuredActivity: "構造化アクティビティはまだありません", structuredToolLog: "ツールタイムライン", diffArtifacts: "Diff 成果物", changedItems: "件の変更", currentTool: "現在のツール", structuredLogPrivacy: "秘匿化された構造化状態のみを表示し、Prompt、ツール引数、出力、内部エラーは含みません。",
  languageName: "日本語", welcomeTitle: "準備できました。どこから始めますか？",
  welcomeBody: "ワークスペースを選び、実装、修正、調査したいことを入力してください。",
  promptPlaceholder: "完了したいタスクを入力…", send: "送信", working: "処理中",
  loadFailed: "読み込みに失敗しました", requestFailed: "リクエストに失敗しました", noSessions: "このワークスペースには履歴がありません",
  emptyReply: "Harness からテキストが返されませんでした",
  stop: "生成を停止", sendHint: "Enter で送信 · Shift+Enter で改行", commands: "コマンド",
  thinking: "思考中", reconnecting: "実行中のセッションに再接続しています", streamDisconnected: "イベントストリームが切断されました", stopping: "停止中", stopped: "停止しました", toolRunning: "実行中", toolDone: "完了", toolFailed: "失敗",
  attachments: "添付ファイル", removeAttachment: "添付を削除", pastedText: "貼り付けたテキスト", pastedImage: "貼り付けた画像", attachmentPrompt: "添付ファイルを分析してください", attachmentCount: "件の添付",
  helpText: "コマンド：/help、/goal <目標>、/goal off、/compress、/skills、/clear。$ でスキルを選択できます。", cleared: "チャット表示を消去しました", goalSet: "ゴールモードを有効にしました", goalOff: "ゴールモードを無効にしました",
  skills: "スキル", noSkills: "このワークスペースにはスキルがありません",
  searchSessions: "セッションを検索", renameSession: "名前を変更", forkSession: "フォーク", archiveSession: "アーカイブ", unarchiveSession: "アーカイブ解除", deleteSession: "削除", exportSession: "エクスポート",
  pinSession: "ピン留め", unpinSession: "ピン留め解除", pinned: "ピン留め済み",
  deleteDialogTitle: "セッションを削除", cancel: "キャンセル", confirmDelete: "削除",
  renamePrompt: "新しいセッション名を入力", forkPrompt: "フォークのセッション名を入力", deleteConfirm: "このセッションとローカル履歴を完全に削除しますか？元に戻せません。", archived: "アーカイブ済み", sessionActionFailed: "セッション操作に失敗しました",
};

export const messages: Record<Language, Messages> = { "zh-CN": zhCN, en, ja };
export const languageLabels: Record<Language, string> = {
  "zh-CN": zhCN.languageName, en: en.languageName, ja: ja.languageName,
};

export function detectLanguage(): Language {
  const saved = localStorage.getItem("willdeep.language");
  if (languages.includes(saved as Language)) return saved as Language;
  const browser = navigator.language.toLowerCase();
  if (browser.startsWith("ja")) return "ja";
  if (browser.startsWith("en")) return "en";
  return "zh-CN";
}
