#!/usr/bin/env ruby
# frozen_string_literal: true

# willdeep-config 的 MCP 服务端：stdio JSON-RPC（MCP 2025-06-18），零外部依赖。
#
# 工具契约见 docs/tool-contract.md。两条硬规则：
#   1. 任何异常都在工具边界转成结构化错误，stdio 循环绝不抛异常（宿主会当成服务崩溃）。
#   2. secret 字段的明文只允许出现在 config.reveal_secret 的返回值里，其它工具一律打码。

# 宿主拉起服务端时不保证带 UTF-8 locale（C locale 下 Ruby 默认 US-ASCII），
# stdio 上的中文 JSON 会被当成非法字节。这里自己钉死，不依赖宿主环境。
Encoding.default_external = Encoding::UTF_8
$stdin.set_encoding(Encoding::UTF_8)
$stdout.set_encoding(Encoding::UTF_8)

lib_dir = File.expand_path("lib", __dir__)
$LOAD_PATH.unshift(lib_dir) unless $LOAD_PATH.include?(lib_dir)

require "json"
require "fileutils"
require "toml_lite"
require "config_doc"
require "config_file"
require "config_render"
require "config_schema"
require "provider_models"

module WilldeepConfig
  PROTOCOL_VERSION = "2025-06-18"
  SERVER_NAME = "config"
  SERVER_VERSION = "0.4.0-rc1"
  MASK = "••••"
  ABSENT = :__absent__
  OCTAL_FILE_MODE = 0o600

  # 用来兜住 schema 未收录的键：键名像密钥就一律按密钥处理，宁可多打码。
  SECRET_KEY_HINT = /(?:^|_)(?:api_?key|secret|token|password|passwd|credential)(?:_|$)/i

  class BadArgument < StandardError; end

  module Json
    module_function

    def obj(*pairs)
      h = {}
      pairs.each { |key, value| h[key.to_s] = value }
      h
    end

    def error(code, message, details = nil)
      out = { "ok" => false, "error" => code, "message" => message }
      out["details"] = details unless details.nil?
      out
    end
  end

  class Toolbox
    # schema 里标了 secret 的键名，用来在 diff 里遮值（diff 行不带节上下文，只能按键名判）
    SECRET_FIELD_KEYS = ConfigSchema::GROUPS
                        .flat_map { |g| g[:fields].select { |f| f[:secret] }.map { |f| f[:key] } }
                        .uniq
                        .freeze

    attr_reader :config_path, :backup_dir

    def initialize(config_path: nil, backup_dir: nil)
      @config_path = config_path || default_config_path
      @backup_dir = backup_dir || default_backup_dir
    end

    def call(name, args)
      args = {} unless args.is_a?(Hash)
      case name.to_s
      when "config.schema" then tool_schema(args)
      when "config.snapshot" then tool_snapshot(args)
      when "config.plan" then tool_plan(args)
      when "config.apply" then tool_apply(args)
      when "config.validate" then tool_validate(args)
      when "config.backups" then tool_backups(args)
      when "config.restore" then tool_restore(args)
      when "config.render" then tool_render(args)
      when "config.reveal_secret" then tool_reveal_secret(args)
      when "config.models" then tool_models(args)
      else Json.error("invalid_edits", "未知工具：#{name}")
      end
    rescue BadArgument => e
      Json.error("invalid_edits", e.message)
    rescue SystemCallError => e
      Json.error("internal", "#{e.class}: #{e.message}")
    rescue => e
      Json.error("internal", "#{e.class}: #{e.message}")
    end

    # ==== 路径与环境 ====

    def default_config_path
      env = ENV["WD_CONFIG_FILE"].to_s
      return File.expand_path(env) unless env.strip.empty?
      File.join(Dir.home, ".willdeep", "config.toml")
    end

    def default_backup_dir
      env = ENV["WD_CONFIG_BACKUP_DIR"].to_s
      return File.expand_path(env) unless env.strip.empty?
      File.join(Dir.home, "Library", "Application Support", "WillDeep",
                "plugin-data", "willdeep-config", "backups")
    end

    def resolve_path(args)
      raw = args["path"]
      return config_path if raw.nil? || raw.to_s.strip.empty?
      raise BadArgument, "path 必须是字符串（得到 #{raw.class}）" unless raw.is_a?(String)
      value = raw.strip
      raise BadArgument, "path 必须是绝对路径：#{value}" unless value.start_with?("/")
      File.expand_path(value)
    end

    # ==== 工具 ====

    def tool_models(args)
      section = args["section"]
      unless section.is_a?(String) && section.start_with?("providers.") &&
             ConfigSchema.valid_instance?(section.delete_prefix("providers."))
        return Json.error("invalid_edits", "section 必须是模型提供商节名")
      end
      values = {}
      text, = read_config_text(resolve_path(args))
      TomlLite.parse_document(text)[:entries].each do |entry|
        values[entry.key] = entry.value if entry.section == section
      end
      base = args.key?("api_base") ? args["api_base"] : values["api_base"]
      key = args.key?("api_key") ? args["api_key"] : values["api_key"]
      unless base.is_a?(String) && (key.nil? || key.is_a?(String))
        return Json.error("models_config", "请先填写接口地址；API 密钥必须是字符串")
      end
      ProviderModels.fetch(base, key.to_s)
    rescue TomlLite::ParseError
      Json.error("parse_error", "配置文件语法错误，无法读取提供商")
    end

    def tool_schema(_args)
      sections = ConfigSchema.static_groups.map do |g|
        {
          "name" => g[:section], "title" => g[:title], "description" => g[:description],
          "groupID" => g[:id], "kind" => "static", "editable" => true, "instance" => nil,
          "prefix" => nil, "fields" => g[:fields].map { |f| schema_field(f) }
        }
      end
      collections = ConfigSchema.collection_groups.map do |g|
        {
          "name" => g[:prefix], "title" => g[:title], "description" => g[:description],
          "groupID" => g[:id], "kind" => "collection", "editable" => true, "instance" => nil,
          "prefix" => g[:prefix], "instanceHint" => g[:instanceHint],
          "fields" => g[:fields].map { |f| schema_field(f) }
        }
      end
      known = ConfigSchema.known_providers
      {
        "ok" => true,
        "sections" => sections + collections,
        "providers" => {
          "types" => known[:types].map { |t| { "id" => t[:id], "label" => t[:label], "apiBase" => t[:apiBase] } },
          "subagentProfiles" => known[:subagentProfiles]
        },
        "collections" => ConfigSchema.collection_groups.map do |g|
          {
            "id" => g[:id], "prefix" => g[:prefix], "title" => g[:title],
            "description" => g[:description], "instanceHint" => g[:instanceHint],
            "fields" => g[:fields].map { |f| schema_field(f) }
          }
        end,
        "filePath" => config_path,
        "backupDir" => backup_dir
      }
    end

    def tool_snapshot(args)
      path = resolve_path(args)
      exists = File.file?(path)
      raw = exists ? File.binread(path) : "".dup.force_encoding(Encoding::BINARY)
      text = raw.dup.force_encoding(Encoding::UTF_8)

      entries = nil
      parse_error = nil
      if exists
        begin
          entries = TomlLite.parse_document(text)[:entries]
        rescue TomlLite::ParseError => e
          parse_error = { "line" => line_of(e), "message" => e.message }
        end
      end

      values = {}
      sections_in_file = []
      (entries || []).each do |e|
        (values[e.section] ||= {})[e.key] = e.value
        sections_in_file << e.section unless sections_in_file.include?(e.section)
      end

      sections = ConfigSchema.static_groups.map { |g| static_section(g, values) }
      ConfigSchema.collection_groups.each do |g|
        collection_instances(g, sections_in_file).each do |instance|
          sections << collection_section(g, instance, values)
        end
      end
      unknown_sections(sections_in_file).each { |name| sections << unknown_section(name, values) }

      {
        "ok" => true,
        "path" => path,
        "exists" => exists,
        "byteSize" => exists ? File.size(path) : 0,
        "mtime" => exists ? File.stat(path).mtime.to_i : nil,
        "lineEnding" => line_ending(raw),
        "trailingNewline" => exists ? raw.end_with?("\n") : false,
        "parseError" => parse_error,
        "sections" => sections,
        "unknownKeys" => unknown_keys(entries || []),
        "collections" => ConfigSchema.collection_groups.map { |g| collection_meta(g) },
        "raw" => masked_raw(exists ? text : "")
      }
    end

    def tool_plan(args)
      path = resolve_path(args)
      edits = args["edits"]
      return Json.error("invalid_edits", "edits 必须是数组") unless edits.is_a?(Array)
      return Json.error("invalid_edits", "edits 不能为空") if edits.empty?

      text, exists = read_config_text(path)
      if exists
        begin
          TomlLite.parse_document(text)
        rescue TomlLite::ParseError => e
          return Json.error("parse_error", "配置文件语法错误，不允许基于坏文件计算变更：#{e.message}")
        end
      end

      doc = ConfigDoc.new(path, text)
      normalized, bad = validate_edits(edits)
      return invalid_edits_result(bad) unless bad.empty?

      changes = []
      begin
        normalized.each { |edit| changes << apply_edit(doc, edit) }
        diff = doc.diff
      rescue ConfigDoc::RenderError => e
        return Json.error("render_failed", e.message)
      end

      Json.obj(
        ["ok", true], ["valid", true], ["path", path],
        ["diff", masked_diff(text, doc.render)], ["changes", changes], ["warnings", []]
      )
    end

    def tool_apply(args)
      path = resolve_path(args)
      return Json.error("invalid_edits", "apply requires confirm=true") unless args["confirm"] == true
      edits = args["edits"]
      return Json.error("invalid_edits", "edits 必须是数组") unless edits.is_a?(Array)
      return Json.error("invalid_edits", "edits 不能为空") if edits.empty?

      text, exists = read_config_text(path)
      base_root = nil
      base_entries = []
      if exists
        begin
          parsed = TomlLite.parse_document(text)
          base_root = parsed[:root]
          base_entries = parsed[:entries]
        rescue TomlLite::ParseError => e
          return Json.error("parse_error", "配置文件语法错误，坏文件不允许盲改：#{e.message}")
        end
      end

      normalized, bad = validate_edits(edits)
      return invalid_edits_result(bad) unless bad.empty?

      doc = ConfigDoc.new(path, text)
      changes = []
      expected = {}
      removed_sections = []
      rendered = nil
      begin
        normalized.each do |edit|
          changes << apply_edit(doc, edit)
          if edit[:op] == :remove_section
            removed_sections << edit[:section]
          else
            expected[[edit[:section], edit[:key]]] = edit[:op] == :set ? edit[:value] : ABSENT
          end
        end
        rendered = doc.render
      rescue ConfigDoc::RenderError => e
        return Json.error("render_failed", e.message)
      end

      diff = masked_diff(text, rendered)
      size_before = exists ? File.size(path) : 0
      backup_path = exists ? ConfigFile.backup(path, backup_dir) : nil

      begin
        ConfigFile.write_atomic(path, rendered, mode: OCTAL_FILE_MODE)
      rescue SystemCallError, ArgumentError => e
        return Json.error("write_failed", "写入失败：#{e.message}", { "rolledBack" => false })
      end

      begin
        verify_written!(path, rendered, expected, base_entries, base_root, size_before,
                        removed_sections)
      rescue => e
        rolled_back = rollback(path, backup_path, exists)
        return Json.error("write_failed", "写入后校验失败：#{e.message}",
                          { "rolledBack" => rolled_back, "backupPath" => backup_path })
      end

      Json.obj(
        ["ok", true], ["backupPath", backup_path], ["diff", diff],
        ["byteSizeBefore", size_before], ["byteSizeAfter", File.size(path)],
        ["changes", changes]
      )
    end

    def tool_validate(args)
      path = resolve_path(args)
      text = args["text"]
      if text.nil?
        return Json.error("missing_file", "配置文件不存在：#{path}") unless File.file?(path)
        text = File.binread(path).dup.force_encoding(Encoding::UTF_8)
      elsif !text.is_a?(String)
        return Json.error("invalid_edits", "text 必须是字符串（得到 #{text.class}）")
      end

      errors = []
      warnings = []
      unknown = []
      parsed = nil
      begin
        parsed = TomlLite.parse_document(text)
      rescue TomlLite::ParseError => e
        errors << { "line" => line_of(e), "message" => e.message }
      end

      if parsed
        unknown = unknown_keys(parsed[:entries])
        first_line = {}
        parsed[:entries].each { |e| first_line[[e.section, e.key]] ||= e.line + 1 }
        unknown.each do |item|
          warnings << { "line" => first_line[[item["section"], item["key"]]],
                        "message" => "schema 未收录的键：#{key_label(item['section'], item['key'])}（本插件不会改动它）" }
        end
      end

      Json.obj(
        ["ok", true], ["path", path], ["valid", errors.empty?],
        ["errors", errors], ["warnings", warnings], ["unknownKeys", unknown]
      )
    end

    def tool_backups(_args)
      items = ConfigFile.list_backups(backup_dir).map do |item|
        { "name" => item[:name], "byteSize" => item[:size],
          "mtime" => item[:mtime].to_i, "path" => item[:path] }
      end
      Json.obj(["ok", true], ["dir", backup_dir], ["items", items])
    end

    def tool_restore(args)
      return Json.error("invalid_edits", "restore requires confirm=true") unless args["confirm"] == true
      name = args["name"].to_s.strip
      return Json.error("invalid_edits", "name 不能为空") if name.empty?
      if name.include?("/") || name.include?("\\")
        return Json.error("invalid_edits", "name 必须是备份文件名，不能包含路径分隔符")
      end

      path = resolve_path(args)
      available = ConfigFile.list_backups(backup_dir).map { |item| item[:name] }
      unless available.include?(name)
        return Json.error("restore_failed", "备份 #{name} 不存在")
      end

      begin
        previous = ConfigFile.restore(File.join(backup_dir, name), path)
      rescue SystemCallError => e
        return Json.error("restore_failed", e.message)
      end

      Json.obj(
        ["ok", true], ["restoredFrom", name],
        ["previousBackupPath", previous], ["byteSize", File.size(path)]
      )
    end

    def tool_render(args)
      overrides = args["overrides"]
      overrides = {} if overrides.nil?
      return Json.error("invalid_edits", "overrides 必须是对象") unless overrides.is_a?(Hash)
      bad = validate_overrides(overrides)
      return invalid_edits_result(bad) unless bad.empty?

      text = begin
        ConfigRender.merge(ConfigSchema.template, overrides)
      rescue ArgumentError => e
        return Json.error("render_failed", e.message)
      end

      begin
        TomlLite.parse_document(text)
      rescue TomlLite::ParseError => e
        return Json.error("render_failed", "生成的模板无法解析：#{e.message}")
      end

      path = resolve_path(args)
      base_text = read_config_text(path).first
      Json.obj(["ok", true], ["text", text], ["diff", masked_diff(base_text, text)])
    end

    def tool_reveal_secret(args)
      return Json.error("invalid_edits", "reveal_secret requires confirm=true") unless args["confirm"] == true
      section = args["section"].to_s.strip
      key = args["key"].to_s.strip
      return Json.error("invalid_edits", "section/key 不能为空") if key.empty?

      field = ConfigSchema.field_def(section, key)
      unless field && field[:secret]
        return Json.error("invalid_edits", "#{key_label(section, key)} 不是 schema 里的 secret 字段")
      end

      path = resolve_path(args)
      return Json.error("missing_file", "配置文件不存在：#{path}") unless File.file?(path)
      text = File.binread(path).dup.force_encoding(Encoding::UTF_8)
      begin
        root = TomlLite.parse(text)
      rescue TomlLite::ParseError => e
        return Json.error("parse_error", e.message)
      end

      found, value = ConfigDoc.lookup(root, TomlLite.split_section_name(section) + [key])
      Json.obj(["ok", true], ["value", found && value.is_a?(String) ? value : ""])
    end

    # ==== 编辑校验与执行 ====

    def validate_edits(edits)
      normalized = []
      bad = []
      edits.each_with_index do |raw, index|
        unless raw.is_a?(Hash)
          bad << { "index" => index, "reason" => "edit 必须是对象（得到 #{raw.class}）" }
          next
        end
        op = raw["op"].to_s
        unless %w[set unset removeSection].include?(op)
          bad << { "index" => index, "reason" => "op 必须是 set、unset 或 removeSection（得到 #{raw['op'].inspect}）" }
          next
        end
        if op == "removeSection"
          why = remove_section_rejection(raw["section"])
          if why
            bad << { "index" => index, "reason" => why }
          else
            normalized << { op: :remove_section, section: raw["section"].strip, key: nil }
          end
          next
        end
        section = raw["section"]
        key = raw["key"]
        unless section.is_a?(String) && key.is_a?(String) && !key.strip.empty?
          bad << { "index" => index, "reason" => "section/key 必须是非空字符串" }
          next
        end
        section = section.strip
        key = key.strip
        reason = section_rejection(section)
        if reason
          bad << { "index" => index, "reason" => reason }
          next
        end
        if ConfigSchema.field_def(section, key).nil?
          bad << { "index" => index, "reason" => "schema 未收录键 #{key_label(section, key)}" }
          next
        end
        if op == "set"
          unless raw.key?("value")
            bad << { "index" => index, "reason" => "set 缺少 value" }
            next
          end
          value = raw["value"]
          why = ConfigSchema.value_errors(section, key, value)
          unless why.empty?
            bad << { "index" => index, "reason" => "#{key_label(section, key)}：#{why.join('；')}" }
            next
          end
          normalized << { op: :set, section: section, key: key, value: value }
        else
          unless raw["value"].nil?
            bad << { "index" => index, "reason" => "unset 不接受 value" }
            next
          end
          normalized << { op: :unset, section: section, key: key }
        end
      end
      # 同一批里既删整节又改节内键，意图自相矛盾，一律退回让页面说清楚
      removed = normalized.select { |e| e[:op] == :remove_section }.map { |e| e[:section] }
      normalized.each_with_index do |edit, index|
        next if edit[:op] == :remove_section || !removed.include?(edit[:section])
        bad << { "index" => index, "reason" => "节 #{edit[:section]} 同时被删除与修改" }
      end
      [normalized, bad]
    end

    # 只有集合实例能整节删除：静态节是 WillDeep 的固定结构，未知节本插件不碰
    def remove_section_rejection(section)
      return "removeSection 需要非空的 section" unless section.is_a?(String) && !section.strip.empty?
      section = section.strip
      collection = ConfigSchema.split_collection(section)
      return "只能删除集合实例（如 providers.<id>），#{section} 不是" unless collection
      section_rejection(section)
    end

    # 返回 nil 表示可以写回，否则是拒绝原因
    def section_rejection(section)
      if (collection = ConfigSchema.split_collection(section))
        return nil if ConfigSchema.valid_instance?(collection[:instance])
        return "节名 #{section} 的实例名非法（不能含点号、方括号、引号或空白），拒绝写回"
      end
      return nil if ConfigSchema.static_groups.any? { |g| g[:section] == section }
      "schema 未收录节 #{section}，拒绝写入（未知内容不会被本插件改动）"
    end

    def validate_overrides(overrides)
      bad = []
      overrides.keys.each_with_index do |raw_section, index|
        section = raw_section.to_s.strip
        kv = overrides[raw_section]
        unless kv.is_a?(Hash)
          bad << { "index" => index, "reason" => "overrides.#{section} 必须是对象" }
          next
        end
        reason = section_rejection(section)
        if reason
          bad << { "index" => index, "reason" => reason }
          next
        end
        kv.each do |raw_key, value|
          key = raw_key.to_s.strip
          if ConfigSchema.field_def(section, key).nil?
            bad << { "index" => index, "reason" => "schema 未收录键 #{key_label(section, key)}" }
            next
          end
          why = ConfigSchema.value_errors(section, key, value)
          bad << { "index" => index, "reason" => "#{key_label(section, key)}：#{why.join('；')}" } unless why.empty?
        end
      end
      bad
    end

    def apply_edit(doc, edit)
      if edit[:op] == :remove_section
        unless doc.section_present?(edit[:section])
          raise ConfigDoc::RenderError, "节 #{edit[:section]} 不存在，无法删除"
        end
        doc.remove_section(edit[:section])
        return {
          "op" => "removeSection", "section" => edit[:section], "key" => nil,
          "from" => nil, "to" => nil, "masked" => false
        }
      end
      section = edit[:section]
      key = edit[:key]
      secret = secret_key?(section, key)
      present_before = doc.key?(section, key)
      before = present_before ? doc.get(section, key) : nil
      if edit[:op] == :set
        doc.set(section, key, edit[:value])
        after = edit[:value]
      else
        doc.unset(section, key)
        after = nil
      end
      {
        "op" => edit[:op].to_s, "section" => section, "key" => key,
        "from" => secret ? (present_before ? MASK : nil) : before,
        "to" => secret ? (after.nil? ? nil : MASK) : after,
        "masked" => secret
      }
    end

    def invalid_edits_result(bad)
      Json.error("invalid_edits", "有 #{bad.length} 条编辑不合法",
                 { "edits" => bad })
    end

    # ==== 写入后校验与回滚 ====

    def verify_written!(path, rendered, expected, base_entries, base_root, size_before,
                        removed_sections = [])
      actual = File.binread(path)
      if actual.bytesize != rendered.bytesize
        raise "落盘字节数与渲染结果不一致（#{actual.bytesize} != #{rendered.bytesize}）"
      end
      limit = [1_048_576, size_before * 4 + 1_048_576].max
      raise "写入后文件大小异常（#{actual.bytesize} 字节）" if actual.bytesize > limit

      text = actual.dup.force_encoding(Encoding::UTF_8)
      raise "写入后不是合法 UTF-8 字节序列" unless text.valid_encoding?
      parsed = begin
        TomlLite.parse_document(text)
      rescue TomlLite::ParseError => e
        raise "写入后无法解析：#{e.message}"
      end

      expected.each do |(section, key), want|
        found, value = ConfigDoc.lookup(parsed[:root], TomlLite.split_section_name(section) + [key])
        label = key_label(section, key)
        if want == ABSENT
          raise "#{label} 未被删除" if found
        elsif !found || !ConfigDoc.value_eql?(value, want)
          raise "#{label} 写入后不等于期望值"
        end
      end

      removed_sections.each do |section|
        found, = ConfigDoc.lookup(parsed[:root], TomlLite.split_section_name(section))
        raise "节 #{section} 未被删除" if found
      end

      seen = {}
      base_entries.each do |entry|
        pair = [entry.section, entry.key]
        next if expected.key?(pair) || seen[pair]
        next if removed_sections.any? { |s| entry.section == s || entry.section.start_with?(s + ".") }
        seen[pair] = true
        before_found, before_value = ConfigDoc.lookup(base_root, entry.section_parts + [entry.key])
        after_found, after_value = ConfigDoc.lookup(parsed[:root], entry.section_parts + [entry.key])
        unless before_found == after_found && ConfigDoc.value_eql?(before_value, after_value)
          raise "未改动的键 #{key_label(entry.section, entry.key)} 值发生了变化"
        end
      end
      true
    end

    def rollback(path, backup_path, existed)
      if backup_path && File.file?(backup_path)
        ConfigFile.write_atomic(path, File.binread(backup_path), mode: OCTAL_FILE_MODE)
        true
      elsif !existed && File.file?(path)
        File.unlink(path)
        true
      else
        false
      end
    rescue SystemCallError
      false
    end

    # ==== 快照组装 ====

    def static_section(group, values)
      {
        "name" => group[:section], "title" => group[:title],
        "description" => group[:description], "groupID" => group[:id],
        "kind" => "static", "editable" => true, "instance" => nil,
        "fields" => group[:fields].map { |f| snapshot_field(group[:section], f, values) }
      }
    end

    def collection_instances(group, sections_in_file)
      prefix = group[:prefix] + "."
      sections_in_file.select { |name| name.start_with?(prefix) && name.length > prefix.length }
                      .map { |name| name[prefix.length..-1] }
                      .sort
    end

    def collection_section(group, instance, values)
      name = "#{group[:prefix]}.#{instance}"
      {
        "name" => name, "title" => instance, "description" => group[:description],
        "groupID" => group[:id], "kind" => "collection",
        "editable" => ConfigSchema.valid_instance?(instance), "instance" => instance,
        "fields" => group[:fields].map { |f| snapshot_field(name, f, values) }
      }
    end

    # 页面「新增实例」要用的信息：放在快照里，页面不必再多调一个命令
    def collection_meta(group)
      {
        "id" => group[:id], "prefix" => group[:prefix], "title" => group[:title],
        "description" => group[:description], "instanceHint" => group[:instanceHint],
        "suggestions" => group[:suggestions] || [],
        "fields" => group[:fields].map { |f| schema_field(f) }
      }
    end

    def unknown_sections(sections_in_file)
      sections_in_file.reject { |name| known_section?(name) }
    end

    def known_section?(name)
      return true if ConfigSchema.static_groups.any? { |g| g[:section] == name }
      !ConfigSchema.split_collection(name).nil?
    end

    def unknown_section(name, values)
      block = values[name] || {}
      {
        "name" => name,
        "title" => name.empty? ? "其他顶层键" : name,
        "description" => "未收录的自定义节，只读展示",
        "groupID" => nil, "kind" => "unknown", "editable" => false, "instance" => nil,
        "fields" => block.keys.map { |key| unknown_field(name, key, block[key]) }
      }
    end

    def unknown_field(section, key, value)
      secret = secret_key?(section, key)
      field = {
        "key" => key, "label" => key, "type" => infer_type(value),
        "secret" => secret, "value" => secret ? "" : value,
        "hasValue" => true, "source" => "file", "options" => nil, "description" => nil
      }
      field["preview"] = secret ? preview_of(value.to_s) : nil
      field
    end

    def snapshot_field(section, field, values)
      block = values[section] || {}
      present = block.key?(field[:key])
      raw_value = present ? block[field[:key]] : nil
      out = {
        "key" => field[:key], "label" => field[:label], "type" => field[:type],
        "secret" => !!field[:secret], "value" => nil, "hasValue" => false,
        "source" => present ? "file" : "default", "options" => field[:options],
        "optionLabels" => field[:optionLabels], "ref" => field[:ref],
        "description" => field[:description]
      }
      if field[:secret]
        has_value = present && !raw_value.to_s.strip.empty?
        out["value"] = ""
        out["hasValue"] = has_value
        out["preview"] = has_value ? preview_of(raw_value.to_s) : nil
      else
        out["value"] = present ? raw_value : field[:default]
        out["hasValue"] = present
      end
      out
    end

    def unknown_keys(entries)
      out = []
      seen = {}
      entries.each do |entry|
        known = ConfigSchema.fields_for(entry.section).any? { |f| f[:key] == entry.key }
        next if known
        pair = [entry.section, entry.key]
        next if seen[pair]
        seen[pair] = true
        out << { "section" => entry.section, "key" => entry.key }
      end
      out
    end

    def schema_field(field)
      {
        "key" => field[:key], "label" => field[:label], "type" => field[:type],
        "secret" => !!field[:secret], "default" => field[:default],
        "description" => field[:description], "options" => field[:options],
        "optionLabels" => field[:optionLabels], "ref" => field[:ref]
      }
    end

    def infer_type(value)
      case value
      when true, false then "bool"
      when Integer then "int"
      when Float then "float"
      when Array then "stringArray"
      else "string"
      end
    end

    # ==== 打码 ====

    def secret_key?(section, key)
      !!(ConfigSchema.secret?(section, key) || key.to_s =~ SECRET_KEY_HINT)
    end

    def preview_of(value)
      return nil if value.to_s.length < 8
      "…" + value.to_s[-4, 4]
    end

    # raw 预览：整份文本，但 secret 键的值换成掩码。用行内替换，行数不变。
    def masked_raw(text)
      return "" if text.nil? || text.empty?
      lines, = ConfigDoc.split_text(text)
      begin
        TomlLite.parse_document(text)[:entries].each do |entry|
          next unless entry.line == entry.end_line
          next unless secret_key?(entry.section, entry.key)
          line = lines[entry.line]
          next if line.nil?
          start_col = entry.value_start_col
          end_col = entry.value_end_col
          next if start_col.nil? || end_col.nil? || end_col < start_col || end_col > line.length
          lines[entry.line] = line[0...start_col].to_s + "\"#{MASK}\"" + line[end_col..-1].to_s
        end
      rescue TomlLite::ParseError
        lines = lines.map { |line| mask_line_fallback(line) }
      end
      out = lines.join("\n")
      out += "\n" if text.end_with?("\n") && !out.empty?
      out
    end

    # 坏文件没法定位值区间，退化成按键名匹配整行。
    def mask_line_fallback(line)
      line.sub(/\A([ \t]*[A-Za-z0-9_."'\[\]-]*(?:api_?key|secret|token|password|passwd|credential)[A-Za-z0-9_."'\[\]-]*[ \t]*=[ \t]*).+\z/i) do
        Regexp.last_match(1) + "\"#{MASK}\""
      end
    end

    # diff 要在不泄漏明文的前提下显示"这一行变了"，所以先按原文比对，再按键名把值替换成掩码。
    def masked_diff(before_text, after_text)
      before_lines = ConfigDoc.split_text(before_text.to_s)[0]
      after_lines = ConfigDoc.split_text(after_text.to_s)[0]
      diff = ConfigDoc.unified_diff(before_lines, after_lines)
      return "" if diff.empty?
      diff.split("\n", -1).map { |line| mask_diff_line(line) }.join("\n")
    end

    def mask_diff_line(line)
      return line if line.empty?
      marker = line[0]
      return line unless marker == "+" || marker == "-" || marker == " "
      content = line[1..-1].to_s
      return line if content.lstrip.start_with?("#")
      match = content.match(/\A([ \t]*)((?:"[^"]*"|[A-Za-z0-9_.-])+)[ \t]*=[ \t]*(.*)\z/)
      return line if match.nil?
      key = match[2].to_s.delete('"')
      return line unless secret_field_key?(key)
      marker + match[1] + match[2] + " = \"#{MASK}\""
    end

    def secret_field_key?(key)
      !!(SECRET_FIELD_KEYS.include?(key.to_s) || key.to_s =~ SECRET_KEY_HINT)
    end

    # ==== 小工具 ====

    def read_config_text(path)
      return ["", false] unless File.file?(path)
      raw = File.binread(path)
      [raw.dup.force_encoding(Encoding::UTF_8), true]
    end

    def line_ending(raw)
      raw.to_s.include?("\r\n") ? "CRLF" : "LF"
    end

    def line_of(error)
      match = error.message.match(/第 (\d+) 行/)
      match ? match[1].to_i : nil
    end

    def key_label(section, key)
      section.to_s.empty? ? key.to_s : "#{section}.#{key}"
    end
  end

  # ==== MCP 协议层 ====

  module ToolSpec
    PATH_PROPERTY = {
      "path" => { "type" => "string",
                  "description" => "配置文件绝对路径；默认取 WD_CONFIG_FILE，再退到 ~/.willdeep/config.toml" }
    }.freeze

    EDITS_PROPERTY = {
      "edits" => {
        "type" => "array",
        "description" => "要应用的键值变更，按顺序执行",
        "items" => {
          "type" => "object",
          "properties" => {
            "op" => { "type" => "string", "enum" => %w[set unset] },
            "section" => { "type" => "string", "description" => "节名；顶层键用空字符串" },
            "key" => { "type" => "string" },
            "value" => { "description" => "set 的新值；unset 不要传" }
          },
          "required" => %w[op section key]
        }
      }
    }.freeze

    class << self
      def all
        [
          tool("config.models", "请求提供商的 /v1/models 列表。使用已保存配置或页面未保存的地址和密钥，不返回凭据。",
               PATH_PROPERTY.merge(
                 "section" => { "type" => "string" },
                 "api_base" => { "type" => "string" },
                 "api_key" => { "type" => "string" }
               )),
          tool("config.snapshot", "读取配置文件快照：按内置 schema 列出节与字段值、未知键、打码后的原文。密钥只给 preview。",
               PATH_PROPERTY),
          tool("config.schema", "返回内置 schema：可编辑的节、字段类型与默认值、提供商候选、文件与备份路径。", {}),
          tool("config.plan", "只计算不写盘：校验 edits 并返回 unified diff 与变更列表。涉及密钥时 diff 与值都打码。",
               EDITS_PROPERTY.merge(PATH_PROPERTY)),
          tool("config.apply", "应用 edits：备份 → 原子写入 0600 → 写入后校验；校验失败会用备份回滚。必须先确认。",
               EDITS_PROPERTY.merge(PATH_PROPERTY).merge(
                 "confirm" => { "type" => "boolean", "description" => "必须为 true，否则拒绝执行" }
               )),
          tool("config.validate", "校验配置文本或文件：语法错误进 errors，schema 未知键进 warnings。",
               PATH_PROPERTY.merge("text" => { "type" => "string", "description" => "给了就校验这段文本，否则校验文件" })),
          tool("config.backups", "列出备份目录里的备份，按时间倒序。", {}),
          tool("config.restore", "用指定备份恢复配置文件；恢复前会先给当前文件做一次备份。必须先确认。",
               { "name" => { "type" => "string", "description" => "备份文件名，来自 config.backups" } }.merge(
                 "confirm" => { "type" => "boolean", "description" => "必须为 true，否则拒绝执行" }
               )),
          tool("config.render", "用内置中文注释模板生成一份完整配置文本（只返回文本，不写盘），overrides 做浅合并。密钥键只写注释提示。",
               { "overrides" => { "type" => "object",
                                  "description" => "形如 {\"agent\":{\"max_turns\":20}}，值必须符合 schema 类型" } }.merge(PATH_PROPERTY)),
          tool("config.reveal_secret", "#{ConfigSchema::SECRET_NEVER_REVEAL}。返回已保存的密钥明文，须 confirm=true。",
               { "section" => { "type" => "string" }, "key" => { "type" => "string" } }.merge(
                 "confirm" => { "type" => "boolean", "description" => "必须为 true，否则拒绝执行" }
               ))
        ]
      end

      def names
        all.map { |spec| spec["name"] }
      end

      private

      def tool(name, description, properties)
        {
          "name" => name,
          "description" => description,
          "inputSchema" => {
            "type" => "object",
            "properties" => properties,
            "required" => [],
            "additionalProperties" => false
          }
        }
      end
    end
  end

  class Server
    def initialize(input: $stdin, output: $stdout, toolbox: nil)
      @input = input
      @output = output
      @toolbox = toolbox || Toolbox.new
    end

    def run
      begin
        @output.binmode if @output.respond_to?(:binmode)
        @output.sync = true if @output.respond_to?(:sync=)
        @input.each_line do |line|
          text = line.strip
          next if text.empty?
          begin
            handle_line(text)
          rescue => e
            respond_error(nil, -32603, "内部错误：#{e.class}: #{e.message}")
          end
        end
      rescue Errno::EPIPE, IOError
        # 宿主关掉了管道：正常退出，不算崩溃。
      end
      0
    end

    def handle_line(text)
      message = begin
        JSON.parse(text)
      rescue JSON::ParserError => e
        return respond_error(nil, -32700, "JSON 解析失败：#{e.message}")
      end
      return unless message.is_a?(Hash)
      handle(message)
    end

    def handle(message)
      id = message["id"]
      method = message["method"].to_s
      params = message["params"].is_a?(Hash) ? message["params"] : {}

      case method
      when "initialize"
        respond(id, {
                  "protocolVersion" => PROTOCOL_VERSION,
                  "capabilities" => { "tools" => { "listChanged" => false } },
                  "serverInfo" => { "name" => SERVER_NAME, "version" => SERVER_VERSION }
                })
      when "ping"
        respond(id, {})
      when "tools/list"
        respond(id, { "tools" => ToolSpec.all })
      when "tools/call"
        call_tool(id, params)
      when "notifications/initialized", "notifications/cancelled", "notifications/roots/list_changed"
        nil
      else
        respond_error(id, -32601, "不支持的方法：#{method}") unless id.nil?
      end
    end

    def call_tool(id, params)
      name = params["name"].to_s
      unless ToolSpec.names.include?(name)
        return respond_error(id, -32602, "未知工具：#{name}")
      end
      args = params["arguments"].is_a?(Hash) ? params["arguments"] : {}
      payload = @toolbox.call(name, args)
      respond(id, {
                "content" => [{ "type" => "text", "text" => JSON.generate(payload) }],
                "isError" => false
              })
    rescue => e
      respond(id, {
                "content" => [{ "type" => "text",
                                "text" => JSON.generate(Json.error("internal", "#{e.class}: #{e.message}")) }],
                "isError" => true
              })
    end

    private

    def respond(id, result)
      write_message("jsonrpc" => "2.0", "id" => id, "result" => result)
    end

    def respond_error(id, code, message)
      write_message("jsonrpc" => "2.0", "id" => id,
                    "error" => { "code" => code, "message" => message })
    end

    def write_message(message)
      @output.write(JSON.generate(message))
      @output.write("\n")
      @output.flush if @output.respond_to?(:flush)
    end
  end
end

if $PROGRAM_NAME == __FILE__
  exit WilldeepConfig::Server.new.run
end
