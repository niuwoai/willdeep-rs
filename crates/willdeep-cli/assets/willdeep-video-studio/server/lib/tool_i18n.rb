# frozen_string_literal: true

# 工具与参数的中文名（0.41.0-rc6），随 tools/list 放在每个工具的 `_meta["willdeep/i18n"]` 里。
#
# 起因：WillDeep 主会话的确认卡把插件工具原样摆出来——「Agent 准备 mcp__video-studio__episode_generate_frames」，
# 参数一行行是 dramaID、requestID、retryOnBlock、models: [1 items]。宿主自己的工具有中文名表，插件工具没有。
#
# 规矩：
# - 只放在 `_meta`（MCP 留给实现方的扩展位），不进 inputSchema：参数表会原样发给模型接口，
#   多出来的关键字可能被严格的接口拒收。
# - 形状：{ "zh-Hans" => { "title" => "整集出首尾帧", "arguments" => { "dramaID" => "短剧", … } } }。
#   arguments 只列这个工具自己有的参数；对照表里没有的参数不列，宿主退回原名。
# - 名字写给最终用户看，按「做什么」起，不写实现细节。
module ToolI18n
  module_function

  META_KEY = "willdeep/i18n"
  LOCALE = "zh-Hans"

  TITLES = {
    "system.status" => "查看插件运行版本",
    "drama.get_progress" => "查看短剧进度", "drama.get_stage_context" => "读取创作阶段材料", "review.run" => "内容审核",
    "drama.list" => "列出短剧", "drama.get" => "读取短剧", "drama.confirm_plan" => "确认策划并建剧",
    "drama.save_metadata" => "保存短剧信息", "drama.save_character" => "保存角色", "drama.save_episode" => "保存分集",
    "drama.save_shot" => "保存分镜", "drama.save_draft" => "保存草稿", "drama.record_review" => "记录审核结论", "drama.write_with_panel" => "写作并专家审改",
    "video.record_review" => "记录成片审核结论", "drama.acknowledge_review" => "知悉审核提醒", "video.acknowledge_review" => "知悉成片审核提醒",
    "drama.list_history" => "查看版本历史", "drama.save_canon" => "保存全剧设定账本", "drama.check_consistency" => "检查前后一致性",
    "drama.read_version" => "读取历史版本", "drama.restore_version" => "恢复历史版本", "drama.list_shots" => "列出分镜",
    "drama.save_shot_drafts" => "批量保存分镜草稿", "drama.save_episode_drafts" => "批量保存分集草稿",
    "drama.commit_episode_drafts" => "采用分集草稿", "drama.commit_draft" => "采用草稿", "drama.discard_draft" => "丢弃草稿",
    "skill.list" => "列出创作技能", "skill.get" => "读取创作技能", "skill.save" => "保存创作技能", "skill.reset" => "恢复默认技能",
    "skill.compose" => "组合创作技能", "chat.append" => "记录共创对话", "chat.list" => "读取共创对话", "chat.clear" => "清空共创对话",
    "drama.record_image" => "登记分镜图片", "drama.record_character_image" => "登记定妆图", "image.generate" => "出图",
    "drama.select_character_image" => "选定定妆图", "drama.select_image" => "选定首尾帧", "drama.select_video" => "选定成片",
    "drama.set_clip_trim" => "设置成片裁剪", "video.status" => "查看视频服务状态", "video.create_draft" => "创建视频草稿",
    "video.list" => "列出视频任务", "video.generate" => "生成视频", "video.refresh" => "刷新视频任务",
    "video.refresh_active" => "刷新进行中的视频", "video.retry" => "重试视频任务", "video.download" => "下载成片",
    "video.remove" => "移入回收站", "video.restore" => "从回收站恢复", "video.settings" => "修改视频设置",
    "video.pick_reference" => "选择参考图", "video.prepare_playback" => "准备播放", "video.reveal_output" => "在访达中显示成片",
    "drama.list_assets" => "列出资产", "drama.get_asset" => "读取资产", "drama.save_asset" => "保存资产",
    "drama.archive_asset" => "归档资产", "drama.record_asset_media" => "登记资产图片", "drama.select_asset_media" => "选定资产图片",
    "drama.set_reference_package" => "设置镜头参考包", "drama.preview_reference_package" => "预览镜头参考包",
    "drama.select_dialogue_audio" => "选定台词配音", "drama.export_manifest" => "导出交接清单", "voice.generate" => "配音",
    "episode.compose_plan" => "查看成片合成计划", "episode.save_compose_settings" => "保存合成设置", "episode.compose" => "合成整集",
    "episode.compose_status" => "查看合成进度", "episode.compose_cancel" => "取消合成", "episode.generate_music" => "生成背景音乐",
    "episode.import_music" => "导入背景音乐", "episode.reveal_output" => "在访达中显示整集", "video.capabilities" => "查看视频能力",
    "review.get_material" => "读取审核材料", "media.read" => "读取媒体文件", "episode.generate_frames" => "整集出首尾帧",
    "episode.dub" => "整集配音", "episode.generate_videos" => "整集生成视频", "review.run_batch" => "批量审核",
    "jobs.status" => "查看后台任务", "jobs.wait" => "等待后台任务", "jobs.cancel" => "取消后台任务", "video.remediate" => "补救成片",
    "episode.remediate_videos" => "整集补救成片", "qa.lessons" => "查看质检经验", "qa.save_lesson" => "保存质检经验",
    "image.qa" => "候选图质检", "episode.accept_recommended_frames" => "整集选用推荐首帧"
  }.freeze

  ARGUMENTS = {
    "expectedContent" => "保存前的文字快照", "expectedCanonRev" => "期望账本版本号",
    "brief" => "创作要求", "maxRevisions" => "最多修订次数", "regenerate" => "重新出初稿",
    "dramaID" => "短剧", "episodeID" => "分集", "shotID" => "镜头", "assetID" => "资产", "characterID" => "角色",
    "requestID" => "请求编号（防重复计费）", "id" => "编号", "jobID" => "任务", "jobIDs" => "任务", "scope" => "范围", "kind" => "类型",
    "model" => "模型", "models" => "模型", "aspect" => "审核方面", "dryRun" => "只预览不执行", "force" => "强制重做",
    "candidateID" => "候选", "candidateIDs" => "候选", "stage" => "创作阶段", "prompt" => "提示词", "shotOrders" => "镜号",
    "title" => "标题", "async" => "后台执行", "basis" => "依据指纹", "target" => "目标", "expectedRev" => "期望版本号",
    "text" => "文本", "duration" => "时长（秒）", "filePath" => "文件路径", "category" => "类别", "categories" => "类别",
    "castIDs" => "出场角色", "autoQA" => "自动质检", "expectedDraftRev" => "期望草稿版本号", "source" => "来源", "commit" => "直接采用",
    "mode" => "模式", "note" => "备注", "notes" => "备注", "sceneID" => "场景", "inferenceSteps" => "推理步数", "mediaURL" => "媒体地址",
    "referenceImagePath" => "参考图", "summary" => "概要", "onlyMissing" => "只补缺的", "versionID" => "版本",
    "mediaImages" => "附带图片", "mediaVideos" => "附带视频", "review" => "审核结论", "common" => "通用技能",
    "acknowledgedBy" => "确认人", "includeItems" => "包含逐项结果", "revoke" => "撤销", "maxRetries" => "最多重试次数",
    "width" => "宽", "height" => "高", "extraDirectives" => "补充要求", "countPerModel" => "每个模型出几张", "name" => "名称",
    "presetID" => "预设", "trimMargin" => "裁剪余量（秒）", "qaConcurrency" => "同时质检数", "videoConcurrency" => "同时生成视频数",
    "imageAutoQA" => "候选图自动质检", "imageAutoSelect" => "自动选定推荐图", "imageRetryOnBlock" => "全被拦截时重出轮数",
    "includeArchived" => "包含已归档", "parentSceneID" => "父场景", "lighting" => "光线", "language" => "语言", "dialect" => "方言",
    "referenceTranscript" => "参考音频文字", "providerVoiceID" => "音色", "presets" => "语气预设", "consent" => "声音授权",
    "archived" => "已归档", "durationMs" => "时长（毫秒）", "package" => "参考包", "purpose" => "用途", "lineID" => "台词",
    "lineIDs" => "台词", "settings" => "设置", "path" => "路径", "fileName" => "文件名", "autoSelect" => "自动选定",
    "retryOnBlock" => "全被拦截时重出轮数", "skipFresh" => "跳过结论仍有效的", "state" => "状态", "limit" => "数量上限",
    "timeoutSeconds" => "最多等待（秒）", "lessonID" => "经验", "applyAs" => "应用方式", "enabled" => "启用", "createdBy" => "创建者",
    "onlyPassing" => "只选通过的", "replaceSelected" => "替换已选", "reviewModel" => "审核模型", "maxSteps" => "最多列几步",
    "episodeOrder" => "第几集", "detail" => "详细程度", "from" => "起", "to" => "止", "plan" => "策划", "aspectRatio" => "画幅",
    "episodeDurationSeconds" => "每集时长（秒）", "description" => "描述", "visualPrompt" => "形象描述", "identityVersion" => "形象版本",
    "script" => "剧本", "order" => "序号", "dialogue" => "台词", "actionStart" => "开场动作", "actionEnd" => "收尾动作",
    "cameraIntent" => "运镜", "soundscape" => "环境声", "music" => "配乐", "caption" => "画面字幕", "startPrompt" => "首帧描述",
    "endPrompt" => "尾帧描述", "draft" => "草稿", "canon" => "设定账本", "create" => "新建", "expectedEpisodeRev" => "期望分集版本号",
    "expectedEpisodeDraftRev" => "期望分集草稿版本号", "expectedShots" => "期望分镜版本", "shots" => "分镜", "episodes" => "分集",
    "orders" => "序号", "body" => "正文", "message" => "消息", "videoID" => "成片", "qaOverride" => "人工放行质检",
    "qaOverrideBy" => "放行人", "inSeconds" => "入点（秒）", "outSeconds" => "出点（秒）", "clear" => "清除", "reason" => "原因",
    "status" => "状态", "query" => "查询", "draftID" => "草稿", "negativePrompt" => "反向提示词", "seed" => "随机种子",
    "assistantProviderID" => "策划模型服务", "assistantModel" => "策划模型", "reviewProviderID" => "审核模型服务", "uiLocale" => "界面语言",
    "videoModel" => "视频模型", "autoDownload" => "自动下载", "autoRemediate" => "自动补救", "remediateMaxRetries" => "补救最多次数",
    "remediateCategories" => "补救的问题类别", "remediateAutoOverride" => "补救不成自动放行", "autoTrim" => "自动裁剪",
    "trimMinSeconds" => "裁剪后至少保留（秒）", "aspects" => "只审这些方面", "panelReview" => "专家席审稿", "panel" => "专家发言记录",
    "holdFull" => "留白镜头（保留全长）", "panelRoundtable" => "专家席走宿主圆桌", "panelRounds" => "圆桌讨论轮数",
    "via" => "结论来源", "roundtable" => "圆桌会话"
  }.freeze

  # 给 tools/list 里的一个工具加上中文名。已有 _meta 的保留，只合并这一项。
  def annotate(tool)
    name = tool[:name] || tool["name"]
    title = TITLES[name]
    properties = (tool[:inputSchema] || tool["inputSchema"] || {})
    properties = properties[:properties] || properties["properties"] || {}
    labels = properties.keys.map(&:to_s).each_with_object({}) { |key, result| result[key] = ARGUMENTS[key] if ARGUMENTS.key?(key) }
    return tool if title.nil? && labels.empty?

    entry = { "title" => title, "arguments" => labels.empty? ? nil : labels }.reject { |_key, value| value.nil? }
    meta = (tool[:_meta] || tool["_meta"] || {}).merge(META_KEY => { LOCALE => entry })
    tool.merge(_meta: meta)
  end
end
