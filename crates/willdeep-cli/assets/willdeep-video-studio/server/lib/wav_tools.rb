# frozen_string_literal: true

require "securerandom"

# 纯 Ruby 的 WAV 处理：读头、拼接、写头。
#
# 用途只有一个：把一镜里逐句生成的对白音频拼成一条音轨，作为 Hub ref2va「图 + 音频」
# 组合的 <Audio 1>。插件服务端是 /usr/bin/ruby 2.6，没有 ffmpeg 也不能装库，而同格式
# PCM 的拼接只是把 data 块首尾相接再重写头，纯 Ruby 就够。格式不一致（采样率、声道、
# 位深不同）直接报错，不做重采样。
module WavTools
  module_function

  class Error < StandardError; end

  Info = Struct.new(:channels, :sample_rate, :bits, :byte_rate, :data_offset, :data_size, :duration_ms, :format_tag)

  def info(path)
    binary = File.binread(path.to_s)
    parse(binary)
  rescue SystemCallError, IOError => error
    raise Error, "cannot read #{File.basename(path.to_s)}: #{error.class}"
  end

  def parse(binary)
    raise Error, "not a RIFF/WAVE file" unless binary.is_a?(String) && binary.bytesize > 44 && binary[0, 4] == "RIFF".b && binary[8, 4] == "WAVE".b

    offset = 12
    fmt = nil
    data_offset = nil
    data_size = nil
    while offset + 8 <= binary.bytesize
      chunk_id = binary[offset, 4]
      chunk_size = binary[offset + 4, 4].unpack("V").first
      if chunk_id == "fmt ".b && chunk_size >= 16
        tag, channels, rate, byte_rate, _align, bits = binary[offset + 8, 16].unpack("vvVVvv")
        fmt = [tag, channels, rate, byte_rate, bits]
      elsif chunk_id == "data".b
        data_offset = offset + 8
        data_size = [chunk_size, binary.bytesize - data_offset].min
        break
      end
      offset += 8 + chunk_size + (chunk_size.odd? ? 1 : 0)
    end
    raise Error, "fmt or data chunk missing" unless fmt && data_offset
    tag, channels, rate, byte_rate, bits = fmt
    raise Error, "only PCM WAV is supported (format tag #{tag})" unless [1, 0xFFFE].include?(tag)
    raise Error, "invalid byte rate" unless byte_rate.positive?

    Info.new(channels, rate, bits, byte_rate, data_offset, data_size, (data_size.to_f / byte_rate * 1000).round, tag)
  end

  # 只算不写：拼接后的时长与字节数，以及格式是否一致。preview 用它，不落文件。
  def plan(paths, gap_ms: 300)
    infos = Array(paths).map { |path| [path, info(path)] }
    raise Error, "no audio files" if infos.empty?
    first = infos.first[1]
    mismatch = infos.reject { |_, entry| entry.channels == first.channels && entry.sample_rate == first.sample_rate && entry.bits == first.bits }
    unless mismatch.empty?
      names = mismatch.map { |path, _| File.basename(path.to_s) }.join(", ")
      raise Error, "audio formats differ (#{names}); regenerate them with the same voice settings"
    end
    gap_bytes = infos.length > 1 ? gap_ms * first.byte_rate / 1000 : 0
    data_bytes = infos.sum { |_, entry| entry.data_size } + gap_bytes * (infos.length - 1)
    { "files" => infos.length, "durationMs" => (data_bytes.to_f / first.byte_rate * 1000).round, "bytes" => data_bytes + 44,
      "sampleRate" => first.sample_rate, "channels" => first.channels, "bits" => first.bits }
  end

  # 拼接并写到 output_path。返回 { "filePath", "durationMs", "bytes" }。
  def concat(paths, output_path, gap_ms: 300)
    planned = plan(paths, gap_ms: gap_ms)
    first = info(paths.first)
    gap = ("\x00".b * (gap_ms * first.byte_rate / 1000))
    # 对齐到帧边界：一帧 = 声道数 × 位深 / 8 字节。
    frame = [first.channels * first.bits / 8, 1].max
    gap = gap[0, gap.bytesize - (gap.bytesize % frame)]
    body = String.new(encoding: Encoding::BINARY)
    Array(paths).each_with_index do |path, index|
      binary = File.binread(path.to_s)
      entry = parse(binary)
      body << gap if index.positive? && !gap.empty?
      body << binary[entry.data_offset, entry.data_size]
    end
    header = "RIFF".b + [36 + body.bytesize].pack("V") + "WAVE".b +
             "fmt ".b + [16, 1, first.channels, first.sample_rate, first.byte_rate, first.channels * first.bits / 8, first.bits].pack("VvvVVvv") +
             "data".b + [body.bytesize].pack("V")
    File.binwrite(output_path.to_s, header + body)
    { "filePath" => File.expand_path(output_path.to_s), "durationMs" => planned["durationMs"], "bytes" => header.bytesize + body.bytesize }
  rescue SystemCallError, IOError => error
    raise Error, "cannot write the dialogue track: #{error.class}"
  end

  def track_name(shot_id)
    "track-#{shot_id.to_s.gsub(/[^A-Za-z0-9]/, '')[0, 8]}-#{SecureRandom.hex(6)}.wav"
  end
end
