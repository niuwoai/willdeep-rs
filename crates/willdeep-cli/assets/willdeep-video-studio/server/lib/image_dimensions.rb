# frozen_string_literal: true

# 只读文件头拿图片宽高（PNG / JPEG / WebP），不依赖 ImageMagick 等外部工具。
#
# 用途（0.35.0-rc2）：image.generate 出完首尾帧、场景图后核对画幅方向。模型偶尔
# 无视请求的尺寸，竖屏剧回一张横图；图已经计费，不能丢，只做标记让人或 Agent 重抽。
# 读不出（格式不认识、文件坏了）时返回 nil，调用方按「未知」处理，不报错不标记。
module ImageDimensions
  module_function

  PNG_SIGNATURE = "\x89PNG\r\n\x1A\n".b.freeze
  # JPEG 的 SOF 段（帧头）标记：C0-CF 里除去 C4（DHT）、C8（保留）、CC（DAC）。
  JPEG_SOF_MARKERS = ((0xC0..0xCF).to_a - [0xC4, 0xC8, 0xCC]).freeze
  # JPEG 头部最多扫这么多字节：EXIF 缩略图可能很大，帧头一般在前 1MB 内。
  JPEG_SCAN_LIMIT = 4 * 1024 * 1024
  HEADER_BYTES = 64
  # 宽高比相差不到 5% 视为方图，避免 1024x1000 这种近方图被判成横或竖。
  SQUARE_TOLERANCE = 1.05

  # 返回 [宽, 高] 或 nil。
  def read(path)
    return nil unless path.is_a?(String) && File.file?(path)

    File.open(path, "rb") do |file|
      head = file.read(HEADER_BYTES).to_s.b
      return png(head) if head.start_with?(PNG_SIGNATURE)
      return webp(head) if head[0, 4] == "RIFF" && head[8, 4] == "WEBP"
      return jpeg(file) if head[0, 2] == "\xFF\xD8".b

      nil
    end
  rescue SystemCallError, IOError
    nil
  end

  # "portrait" / "landscape" / "square"。
  def orientation(width, height)
    w = width.to_f
    h = height.to_f
    return "portrait" if h > w * SQUARE_TOLERANCE
    return "landscape" if w > h * SQUARE_TOLERANCE

    "square"
  end

  # "1080x1920" -> [1080, 1920]；格式不对返回 nil。
  def parse_size(text)
    match = text.to_s.match(/\A(\d+)x(\d+)\z/)
    match ? [match[1].to_i, match[2].to_i] : nil
  end

  def png(head)
    return nil unless head[12, 4] == "IHDR"

    width, height = head[16, 8].unpack("NN")
    valid(width, height)
  end

  def webp(head)
    case head[12, 4]
    when "VP8 "
      # 有损：帧头 3 字节 + 起始码 9D 01 2A，之后是 14 位宽高。
      return nil unless head[23, 3] == "\x9D\x01\x2A".b
      width, height = head[26, 4].unpack("vv")
      valid(width & 0x3FFF, height & 0x3FFF)
    when "VP8L"
      return nil unless head.getbyte(20) == 0x2F
      bits = head[21, 4].unpack1("V")
      valid((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1)
    when "VP8X"
      bytes = head[24, 6].bytes
      return nil unless bytes.length == 6
      valid(1 + bytes[0] + (bytes[1] << 8) + (bytes[2] << 16), 1 + bytes[3] + (bytes[4] << 8) + (bytes[5] << 16))
    end
  end

  def jpeg(file)
    file.seek(2)
    while file.pos < JPEG_SCAN_LIMIT
      byte = file.read(1)
      return nil if byte.nil?
      next unless byte.ord == 0xFF

      marker = file.read(1)
      return nil if marker.nil?
      code = marker.ord
      # 填充字节、独立标记（RSTn、TEM、SOI）没有长度字段。
      next if code == 0xFF || code == 0x01 || code == 0xD8 || (0xD0..0xD7).cover?(code)
      return nil if code == 0xD9 || code == 0xDA

      length_bytes = file.read(2)
      return nil if length_bytes.nil? || length_bytes.bytesize < 2
      length = length_bytes.unpack1("n")
      if JPEG_SOF_MARKERS.include?(code)
        data = file.read(5)
        return nil if data.nil? || data.bytesize < 5
        height, width = data[1, 4].unpack("nn")
        return valid(width, height)
      end
      file.seek(length - 2, IO::SEEK_CUR)
    end
    nil
  end

  def valid(width, height)
    width.to_i.positive? && height.to_i.positive? ? [width.to_i, height.to_i] : nil
  end
end
