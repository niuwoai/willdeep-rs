# 内置 schema：哪些节/键是可以被页面编辑的、类型与显示名是什么。
#
# 只收录 ~/.willdeep/config.toml 里真实存在的键。文件里出现而 schema 没有的键一律算
# unknownKeys，页面只读展示、不改不删，避免这个插件猜错了键名就把用户配置搞坏。
#
# default 只在该键的类型本身就隐含默认值时才给（数组 → []），其余为 nil，
# 也就是「本插件不知道 WillDeep 的默认值，面板显示未设置」，不编造。
module ConfigSchema
  TYPES = %w[string int float bool stringArray secret enum].freeze

  SECRET_NEVER_REVEAL = "仅供插件页面显示已保存的密钥，Agent 不要调用"

  ROOT = "".freeze

  # 下面三组取值以 willdeep-rs 的解析器为准（approval.rs `ApprovalMode::parse`、
  # main.rs `parse_provider` / `parse_api`）：写进别的值，rs 启动时直接报错。
  # WillDeep mac 只读 [agent] 的路由开关与 [notifications]，不解析这三个键。
  APPROVAL_MODES = [
    { id: "read-only", label: "只读" },
    { id: "strict", label: "严格：每次都问" },
    { id: "smart", label: "智能：危险操作才问（推荐）" },
    { id: "workspace-write", label: "工作区内放行" },
    { id: "full-access", label: "完全放行" }
  ].freeze

  PROVIDER_KINDS = [
    { id: "auto", label: "自动识别" },
    { id: "some-im", label: "some.im" },
    { id: "openai-compatible", label: "OpenAI 兼容" },
    { id: "anthropic", label: "Anthropic" }
  ].freeze

  API_DIALECTS = [
    { id: "auto", label: "自动" },
    { id: "chat-completions", label: "Chat Completions" },
    { id: "responses", label: "Responses" },
    { id: "anthropic-messages", label: "Anthropic Messages" }
  ].freeze

  def self.labels_of(list)
    list.each_with_object({}) { |m, h| h[m[:id]] = m[:label] }
  end

  # 静态节：名字固定
  STATIC_GROUPS = [
    {
      id: "root", section: ROOT, title: "常规", description: "全局默认值",
      fields: [
        { key: "version", label: "配置版本", type: "int", default: nil },
        { key: "default_provider", label: "默认提供商", type: "string", default: nil, ref: "providers",
          description: "对应 [providers.<id>] 里的 id" }
      ]
    },
    {
      id: "agent", section: "agent", title: "Agent 行为", description: "回合上限、审批与派发策略",
      fields: [
        { key: "max_turns", label: "最大回合数", type: "int", default: nil },
        { key: "approval", label: "审批策略", type: "enum", default: nil,
          options: APPROVAL_MODES.map { |m| m[:id] }, optionLabels: labels_of(APPROVAL_MODES),
          description: "写文件、跑命令、联网前要不要先问你" },
        { key: "small_model_routing", label: "小模型路由", type: "bool", default: nil,
          description: "把轻量任务交给小模型" },
        { key: "auto_dispatch_read_only", label: "只读操作自动派发", type: "bool", default: nil },
        { key: "max_deep_calls_per_harness", label: "单次会话最大深度调用", type: "int", default: nil }
      ]
    },
    {
      id: "local_model", section: "local_model", title: "本地模型", description: "本地推理服务的接入方式",
      fields: [
        { key: "enabled", label: "启用本地模型", type: "bool", default: nil },
        { key: "base_url", label: "服务地址", type: "string", default: nil },
        { key: "summary_model", label: "摘要模型", type: "string", default: nil },
        { key: "prefer_for_titles", label: "标题优先生成", type: "bool", default: nil },
        { key: "prefer_for_context_summaries", label: "上下文摘要优先", type: "bool", default: nil },
        { key: "prefer_for_worker_routing", label: "Worker 路由优先", type: "bool", default: nil }
      ]
    },
    {
      id: "skills", section: "skills", title: "技能", description: "技能搜索路径",
      fields: [
        { key: "roots", label: "技能根目录", type: "stringArray", default: [] }
      ]
    },
    {
      id: "notifications", section: "notifications", title: "通知", description: "提示音与 Webhook 通知",
      fields: [
        { key: "sound", label: "提示音", type: "string", default: nil },
        { key: "custom_sound_file", label: "自定义音频文件", type: "string", default: nil },
        { key: "custom_sound_display_name", label: "自定义音频名称", type: "string", default: nil },
        { key: "webhook_enabled", label: "启用 Webhook", type: "bool", default: nil },
        { key: "webhook_url", label: "Webhook 地址", type: "string", default: "" },
        { key: "webhook_on_task_completed", label: "任务完成时通知", type: "bool", default: nil },
        { key: "webhook_on_attention_required", label: "需要关注时通知", type: "bool", default: nil }
      ]
    }
  ].freeze

  # 集合：节名是 <prefix>.<实例名>，实例名由用户决定，字段模板固定
  COLLECTION_GROUPS = [
    {
      id: "providers", prefix: "providers", title: "模型提供商",
      description: "每个 [providers.<id>] 是一个提供商", instanceHint: "id 用字母数字与连字符",
      fields: [
        { key: "provider", label: "提供商类型", type: "enum", default: nil,
          options: PROVIDER_KINDS.map { |m| m[:id] }, optionLabels: labels_of(PROVIDER_KINDS) },
        { key: "api", label: "接口协议", type: "enum", default: nil,
          options: API_DIALECTS.map { |m| m[:id] }, optionLabels: labels_of(API_DIALECTS) },
        { key: "api_base", label: "接口地址", type: "string", default: nil },
        { key: "api_key", label: "API 密钥", type: "secret", default: nil, secret: true },
        { key: "model", label: "默认模型", type: "string", default: nil },
        { key: "max_output_tokens", label: "最大输出 Token", type: "int", default: nil }
      ]
    },
    {
      id: "mcp_servers", prefix: "mcp_servers", title: "MCP 服务",
      description: "每个 [mcp_servers.<name>] 是一个外部 MCP 服务", instanceHint: "name 用字母数字与连字符",
      fields: [
        { key: "command", label: "启动命令", type: "string", default: nil },
        { key: "args", label: "启动参数", type: "stringArray", default: [] },
        { key: "startup_timeout_seconds", label: "启动超时（秒）", type: "int", default: 10 },
        { key: "enabled", label: "启用", type: "bool", default: true }
      ]
    },
    {
      id: "subagents", prefix: "subagents", title: "子代理",
      description: "每个 [subagents.<profile>] 覆盖该角色的默认参数", instanceHint: "profile 用职责 id",
      suggestions: %w[generalist implementer tester reviewer ops_runner],
      fields: [
        { key: "context_window", label: "上下文窗口", type: "int", default: nil }
      ]
    }
  ].freeze

  GROUPS = (STATIC_GROUPS + COLLECTION_GROUPS).freeze

  # 已知的提供商类型，供页面做候选。api_base 一律不给，避免我们猜错地址。
  KNOWN_PROVIDER_TYPES = PROVIDER_KINDS

  # 已知的子代理职责
  KNOWN_SUBAGENT_PROFILES = COLLECTION_GROUPS.find { |g| g[:id] == "subagents" }[:suggestions].freeze

  class << self
    def static_groups
      STATIC_GROUPS
    end

    def collection_groups
      COLLECTION_GROUPS
    end

    def group(id)
      GROUPS.find { |g| g[:id] == id }
    end

    def collection_by_prefix(prefix)
      COLLECTION_GROUPS.find { |g| g[:prefix] == prefix }
    end

    # "providers.some-im" → {group: <collection>, instance: "some-im"}
    def split_collection(section)
      return nil if section.nil?
      name = section.to_s
      COLLECTION_GROUPS.each do |g|
        pre = g[:prefix] + "."
        return { group: g, instance: name[pre.length..-1] } if name.start_with?(pre) && name.length > pre.length
      end
      nil
    end

    # 某个规范节名对应的字段定义数组；未知节返回 []
    def fields_for(section)
      name = section.to_s
      static = STATIC_GROUPS.find { |g| g[:section] == name }
      return static[:fields] if static
      coll = split_collection(name)
      # 实例名非法时也给出模板字段：调用方按 editable 决定只读，页面不至于渲染成空卡片
      return coll[:group][:fields] if coll
      []
    end

    def field_def(section, key)
      fields_for(section).find { |f| f[:key] == key.to_s }
    end

    def secret?(section, key)
      f = field_def(section, key)
      !!(f && f[:secret])
    end

    # 节名对应的展示信息；未知节返回通用信息
    def section_info(section)
      name = section.to_s
      static = STATIC_GROUPS.find { |g| g[:section] == name }
      if static
        return { name: name, title: static[:title], description: static[:description],
                 groupID: static[:id], kind: "static", editable: true }
      end
      coll = split_collection(name)
      if coll
        return { name: name, title: coll[:instance], description: coll[:group][:description],
                 groupID: coll[:group][:id], kind: "collection", editable: valid_instance?(coll[:instance]) }
      end
      { name: name, title: name.empty? ? "其他顶层键" : name, description: "未收录的自定义节，只读",
        groupID: nil, kind: "unknown", editable: false }
    end

    # 实例名必须是非空、不含点号/方括号/空白的字符串，否则我们没法安全地写回节头
    def valid_instance?(name)
      s = name.to_s
      return false if s.empty? || s.length > 64
      s !~ /[\s\.\[\]"'#]/ && s.valid_encoding?
    end

    def valid_instance_name!(name)
      raise ArgumentError, "实例名不能为空" if name.to_s.strip.empty?
      raise ArgumentError, "实例名含非法字符：#{name.inspect}" unless valid_instance?(name.to_s.strip)
      name.to_s.strip
    end

    def section_name_for(group_id, instance)
      case group_id
      when "providers", "mcp_servers", "subagents"
        "#{group_id}.#{valid_instance_name!(instance)}"
      else
        g = group(group_id)
        raise ArgumentError, "未知分组：#{group_id}" unless g && g[:kind].nil?
        g[:section]
      end
    end

    # 生成新配置文件的模板：每个节带中文注释，secret 键只留注释提示。
    def template
      lines = ["# WillDeep 配置", "# 由 willdeep-config 插件生成", ""]
      STATIC_GROUPS.each do |g|
        next if g[:section] == ROOT && g[:id] == "root"
        lines << "# #{g[:title]}：#{g[:description]}"
        lines << "[#{g[:section]}]"
        g[:fields].each do |f|
          if f[:secret]
            lines << "# #{f[:key]} = \"\"   # #{SECRET_NEVER_REVEAL}"
          elsif blank_default?(f)
            note = f[:description] ? "（#{f[:description]}）" : ""
            note += "；可选：#{f[:options].join(' / ')}" if f[:options]
            lines << "# #{f[:key]} =   # 未设置" + note
          else
            lines << "#{f[:key]} = #{TomlLite.dump_value(f[:default])}"
          end
        end
        lines << ""
      end
      COLLECTION_GROUPS.each do |g|
        lines << "# #{g[:title]}：#{g[:description]}"
        lines << "# [#{g[:prefix]}.<#{g[:instanceHint]}>]"
        g[:fields].each do |f|
          if f[:secret]
            lines << "# #{f[:key]} = \"\"   # #{SECRET_NEVER_REVEAL}"
          end
        end
        lines << ""
      end
      lines.join("\n")
    end

    def known_providers
      {
        types: KNOWN_PROVIDER_TYPES.map { |t| { id: t[:id], label: t[:label], apiBase: nil } },
        subagentProfiles: KNOWN_SUBAGENT_PROFILES
      }
    end

    # 默认值为空（nil / 空串 / 空数组）时模板里只写注释，不替用户落一个值
    def blank_default?(field)
      d = field[:default]
      d.nil? || d == "" || (d.is_a?(Array) && d.empty?)
    end

    # 校验一个 edits 元素里的值是否与 schema 类型相符。返回错误说明数组（空表示通过）
    def value_errors(section, key, value)
      field = field_def(section, key)
      return [] if field.nil? # 未知键由调用方的 unknown-key 策略处理
      case field[:type]
      when "int"
        value.is_a?(Integer) ? [] : ["期望整数，得到 #{value.class}"]
      when "float"
        value.is_a?(Float) || value.is_a?(Integer) ? [] : ["期望数字，得到 #{value.class}"]
      when "bool"
        (value == true || value == false) ? [] : ["期望布尔值，得到 #{value.class}"]
      when "string", "secret"
        return ["期望字符串，得到 #{value.class}"] unless value.is_a?(String)
        return [] unless field[:type] == "secret"
        value.strip.empty? ? ["密钥不能为空字符串（要清空请用 unset）"] : []
      when "enum"
        (field[:options] || []).include?(value) ? [] : ["取值必须是 #{field[:options].inspect} 之一"]
      when "stringArray"
        return ["期望字符串数组，得到 #{value.class}"] unless value.is_a?(Array)
        bad = value.find { |v| !v.is_a?(String) }
        bad ? ["数组元素必须是字符串，得到 #{bad.class}"] : []
      else
        []
      end
    end
  end
end
