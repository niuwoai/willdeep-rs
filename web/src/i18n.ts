export const languages = ["zh-CN", "en", "ja"] as const;
export type Language = (typeof languages)[number];

const zhCN = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "工作区", session: "历史会话", newSession: "新会话", language: "语言",
  languageName: "简体中文", welcomeTitle: "准备好了，从哪里开始？",
  welcomeBody: "选择一个工作区，直接描述你想实现、修复或调查的事情。",
  promptPlaceholder: "描述你想完成的任务…", send: "发送", working: "正在处理",
  loadFailed: "加载失败", requestFailed: "请求失败", noSessions: "这个工作区还没有会话",
  emptyReply: "Harness 没有返回文本",
  stop: "停止生成", sendHint: "Enter 发送 · Shift+Enter 换行", commands: "命令",
  thinking: "正在思考", stopping: "正在停止", stopped: "已停止", toolRunning: "运行中", toolDone: "已完成", toolFailed: "失败",
  attachments: "附件", removeAttachment: "删除附件", pastedText: "粘贴文本", pastedImage: "粘贴图片", attachmentPrompt: "请分析附件", attachmentCount: "个附件",
  helpText: "可用命令：/help、/goal <目标>、/goal off、/compress、/skills、/clear。输入 $ 可选择技能。", cleared: "聊天显示已清空", goalSet: "目标模式已开启", goalOff: "目标模式已关闭",
  skills: "技能", noSkills: "当前工作区没有发现技能",
  searchSessions: "搜索会话", renameSession: "重命名", forkSession: "分叉", archiveSession: "归档", unarchiveSession: "取消归档", deleteSession: "删除", exportSession: "导出",
  renamePrompt: "输入新的会话名称", forkPrompt: "输入分叉会话名称", deleteConfirm: "永久删除这个会话及其本地历史？此操作无法撤销。", archived: "已归档", sessionActionFailed: "会话操作失败",
};

export type Messages = typeof zhCN;

const en: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "Workspace", session: "Sessions", newSession: "New session", language: "Language",
  languageName: "English", welcomeTitle: "Ready. Where should we start?",
  welcomeBody: "Choose a workspace and describe what you want to build, fix, or investigate.",
  promptPlaceholder: "Describe the task you want to complete…", send: "Send", working: "Working",
  loadFailed: "Failed to load", requestFailed: "Request failed", noSessions: "No sessions in this workspace",
  emptyReply: "The harness returned no text",
  stop: "Stop generating", sendHint: "Enter to send · Shift+Enter for newline", commands: "Commands",
  thinking: "Thinking", stopping: "Stopping", stopped: "Stopped", toolRunning: "Running", toolDone: "Done", toolFailed: "Failed",
  attachments: "Attachments", removeAttachment: "Remove attachment", pastedText: "Pasted text", pastedImage: "Pasted image", attachmentPrompt: "Please analyze the attachments", attachmentCount: "attachments",
  helpText: "Commands: /help, /goal <text>, /goal off, /compress, /skills, /clear. Type $ to choose a skill.", cleared: "Chat display cleared", goalSet: "Goal mode enabled", goalOff: "Goal mode disabled",
  skills: "Skills", noSkills: "No skills found in this workspace",
  searchSessions: "Search sessions", renameSession: "Rename", forkSession: "Fork", archiveSession: "Archive", unarchiveSession: "Unarchive", deleteSession: "Delete", exportSession: "Export",
  renamePrompt: "Enter a new session title", forkPrompt: "Enter a title for the fork", deleteConfirm: "Permanently delete this session and its local history? This cannot be undone.", archived: "Archived", sessionActionFailed: "Session action failed",
};

const ja: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "ワークスペース", session: "履歴", newSession: "新しいセッション", language: "言語",
  languageName: "日本語", welcomeTitle: "準備できました。どこから始めますか？",
  welcomeBody: "ワークスペースを選び、実装、修正、調査したいことを入力してください。",
  promptPlaceholder: "完了したいタスクを入力…", send: "送信", working: "処理中",
  loadFailed: "読み込みに失敗しました", requestFailed: "リクエストに失敗しました", noSessions: "このワークスペースには履歴がありません",
  emptyReply: "Harness からテキストが返されませんでした",
  stop: "生成を停止", sendHint: "Enter で送信 · Shift+Enter で改行", commands: "コマンド",
  thinking: "思考中", stopping: "停止中", stopped: "停止しました", toolRunning: "実行中", toolDone: "完了", toolFailed: "失敗",
  attachments: "添付ファイル", removeAttachment: "添付を削除", pastedText: "貼り付けたテキスト", pastedImage: "貼り付けた画像", attachmentPrompt: "添付ファイルを分析してください", attachmentCount: "件の添付",
  helpText: "コマンド：/help、/goal <目標>、/goal off、/compress、/skills、/clear。$ でスキルを選択できます。", cleared: "チャット表示を消去しました", goalSet: "ゴールモードを有効にしました", goalOff: "ゴールモードを無効にしました",
  skills: "スキル", noSkills: "このワークスペースにはスキルがありません",
  searchSessions: "セッションを検索", renameSession: "名前を変更", forkSession: "フォーク", archiveSession: "アーカイブ", unarchiveSession: "アーカイブ解除", deleteSession: "削除", exportSession: "エクスポート",
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
