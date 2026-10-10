# frozen_string_literal: true

# 剧本里的数字：汉字数字（四百八十六、一百二、两万五）与阿拉伯数字（486、４８６、1,200、5万）。
#
# 只服务一致性检查（lib/consistency_check.rb）：把台词里写的数和全剧设定账本对一对。
# 范围 0–99999，够存栏、订单、欠款、价格这一类账；超出范围或写法不规整（「四八六」
# 这种逐位念法、「百」打头）一律认不出，返回 nil——认不出就不报，宁可漏报不误报。
module ChineseNumeral
  MAX = 99_999
  DIGITS = {
    "零" => 0, "〇" => 0, "一" => 1, "二" => 2, "两" => 2, "三" => 3, "四" => 4,
    "五" => 5, "六" => 6, "七" => 7, "八" => 8, "九" => 9
  }.freeze
  UNITS = { "十" => 10, "百" => 100, "千" => 1000 }.freeze
  TEN_THOUSAND = "万"
  CHINESE_CHARS = (DIGITS.keys + UNITS.keys + [TEN_THOUSAND]).join
  # 一段数字：汉字数字串，或阿拉伯数字（含全角、千分位逗号，可带「万 / 千」）。
  TOKEN = /[0-9０-９]+(?:,[0-9]{3})*(?:\.[0-9]+)?[万千]?|[#{CHINESE_CHARS}]+/.freeze

  module_function

  # 一段文字整体解析成数；认不出返回 nil。
  def parse(text)
    token = text.to_s.strip
    return nil if token.empty?
    return parse_arabic(token) if token.match?(/\A[0-9０-９]/)

    parse_chinese(token)
  end

  # 文中所有认得出的数：[{ "value", "text", "start", "end" }]，start / end 是字符下标。
  def scan(text)
    found = []
    text.to_s.to_enum(:scan, TOKEN).each do
      match = Regexp.last_match
      value = parse(match[0])
      next if value.nil?

      found << { "value" => value, "text" => match[0], "start" => match.begin(0), "end" => match.end(0) }
    end
    found
  end

  def parse_arabic(token)
    multiplier = 1
    body = token
    if body.end_with?(TEN_THOUSAND)
      multiplier = 10_000
      body = body[0..-2]
    elsif body.end_with?("千")
      multiplier = 1000
      body = body[0..-2]
    end
    normalized = body.tr("０-９", "0-9").delete(",")
    return nil unless normalized.match?(/\A[0-9]+(?:\.[0-9]+)?\z/)

    value = normalized.include?(".") ? (normalized.to_f * multiplier) : (normalized.to_i * multiplier)
    value = value.to_i if value.is_a?(Float) && value == value.floor
    value.between?(0, MAX) ? value : nil
  end

  # 位值写法：千百十从高到低、「零」只能出现在位之后、末尾单个数字按上一位的十分之一算
  # （一百二 = 120、三千五 = 3500、两万五 = 25000）。
  def parse_chinese(token)
    chars = token.chars
    return 0 if chars.all? { |char| %w[零 〇].include?(char) } && chars.length == 1

    total = 0
    section = 0
    pending = nil
    last_unit = nil
    zero_since_unit = false
    previous = nil
    chars.each_with_index do |char, index|
      if DIGITS.key?(char)
        digit = DIGITS[char]
        if digit.zero?
          # 「零」只能夹在位之间：一百零五、一万零五十。
          return nil if last_unit.nil? || !pending.nil?

          zero_since_unit = true
        else
          return nil unless pending.nil?

          pending = digit
        end
      elsif UNITS.key?(char)
        unit = UNITS[char]
        coefficient = pending
        if coefficient.nil?
          # 「十五」「二十」以外的位不能打头；「一百十」这种口语也认。
          return nil unless unit == 10 && (index.zero? || last_unit)

          coefficient = 1
        end
        small = last_unit && last_unit < 10_000 ? last_unit : nil
        return nil if small && unit >= small

        section += coefficient * unit
        pending = nil
        last_unit = unit
        zero_since_unit = false
      elsif char == TEN_THOUSAND
        section += pending.to_i
        return nil if section.zero? || section >= 10 || total.positive?

        total = section * 10_000
        section = 0
        pending = nil
        last_unit = 10_000
        zero_since_unit = false
      else
        return nil
      end
      previous = char
    end
    if pending
      if last_unit && !zero_since_unit && previous && DIGITS.key?(previous) && last_unit >= 100 &&
         !DIGITS.key?(chars[-2].to_s)
        section += pending * (last_unit / 10)
      else
        section += pending
      end
    end
    value = total + section
    value.between?(0, MAX) ? value : nil
  end
end
