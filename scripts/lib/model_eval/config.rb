# frozen_string_literal: true

module ModelEval
  # 评测用的配置从用户配置派生：原文照抄，只砍掉两张表。
  #
  #   [notifications]   二十个任务乘三个模型，一夜响六十次 webhook，谁也受不了
  #   [mcp_servers.*]   任务用不上外部 MCP，每次启动都去连一遍纯属白等
  #
  # 凭据仍在派生文件里——它只落在私有临时目录（0600），跑完即删，
  # 不进报告、不进仓库、不进日志。这里不解析 TOML，只按表头切段：
  # 凭据行原样搬运，永远不会被单独取出来。
  module Config
    STRIPPED_TABLES = %w[notifications mcp_servers].freeze
    HEADER = /\A\s*\[\[?\s*([^\]]+?)\s*\]\]?\s*(?:#.*)?\z/

    module_function

    def derive(text)
      keep = true
      text.each_line.select do |line|
        header = line.chomp[HEADER, 1]
        keep = !STRIPPED_TABLES.include?(table_of(header)) if header
        keep
      end.join
    end

    # `[providers.some-im]` 的表是 providers；`[mcp_servers."my.server"]` 的表是 mcp_servers。
    def table_of(header)
      header.start_with?('"') ? header[1..].split('"').first : header.split('.').first
    end
  end
end
