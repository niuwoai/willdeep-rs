# frozen_string_literal: true

# 从 willdeep 配置里取默认 provider 的端点与凭据，给花钱的实弹脚本用。
#
# 凭据只在进程内传给 cargo：调用方不得打印、不得写进报告或归档。
module WilldeepCredentials
  module_function

  # 只认默认 provider 那一段的 api_base / api_key。配置里可能有多个 provider，
  # 猜错一个就是拿错凭据打错端点，还不如报错。
  def default_provider(path)
    raise "配置文件不存在：#{path}" unless File.exist?(path)

    # 配置里有中文注释，默认外部编码可能是 US-ASCII，读进来就炸。
    text = File.read(path, encoding: 'UTF-8')
    default = text[/^\s*default_provider\s*=\s*"([^"]+)"/, 1]
    raise "#{path} 里没有 default_provider" unless default

    section = text[/^\s*\[providers\.#{Regexp.escape(default)}\]\s*$(.*?)(?=^\s*\[|\z)/m, 1]
    raise "#{path} 里没有 [providers.#{default}] 段" unless section

    base = section[/^\s*api_base\s*=\s*"([^"]+)"/, 1]
    key = section[/^\s*api_key\s*=\s*"([^"]+)"/, 1]
    key ||= ENV[section[/^\s*api_key_env\s*=\s*"([^"]+)"/, 1].to_s]
    raise "provider #{default} 缺 api_base" unless base
    raise "provider #{default} 缺 api_key（或 api_key_env 指向的环境变量为空）" if key.to_s.strip.empty?

    [default, base, key]
  end
end
