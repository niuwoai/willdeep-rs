# frozen_string_literal: true

require "open3"

require_relative "ffmpeg_tool"

# 画面字幕（0.41.0-rc1）：合成时叠在一镜开头的字，如「第 3 天」。
#
# 起因（《回村养鸭》第 3 集）：分镜把「画面角落字幕：第 3 天」写进了 startPrompt，出图模型照画、
# 视频模型照着首帧继续画，成片画面质检以「烧录叠字」block，两镜只能带着问题放行。天数、地点这类
# 字幕本来就该在合成时叠加：字体、位置、时长都受控，不会写错字，也不占出图 / 视频提示词。
#
# 做法：先把字渲染成一张带半透明圆角底的 PNG，再在逐镜合成时 overlay，开头淡入、结尾淡出。
# 不用 ffmpeg 的 drawtext：Homebrew 的 ffmpeg 不带 libfreetype，没有这个滤镜（见 FFmpegTool.filter?）。
# 渲染器按顺序试：
# 1. macOS 自带的 osascript（JXA + AppKit），系统字体苹方，不依赖任何额外安装；
# 2. ImageMagick（magick），用系统里找得到的中文字体文件。
# 都不行时返回 nil，合成照常进行，只是这一镜不叠字（调用方记 caption_not_rendered 警告）。
module CaptionOverlay
  # 字幕停留时长（秒）与淡入淡出时长；镜头比这短时停到镜头结束。
  DISPLAY_SECONDS = 2.5
  FADE_IN_SECONDS = 0.2
  FADE_OUT_SECONDS = 0.4
  # 字号取成片高度的比例：1280 高约 40 像素。
  FONT_HEIGHT_RATIO = 0.032
  MIN_FONT_PIXELS = 18
  # 位置：左上角，距左边成片宽度的 6%、距顶边高度的 7%（避开平台底部的标题与进度条）。
  LEFT_RATIO = 0.06
  TOP_RATIO = 0.07
  # 底框：半透明黑，内边距按字号算。
  BOX_ALPHA = 0.45
  PAD_X_RATIO = 0.6
  PAD_Y_RATIO = 0.3
  CORNER_RATIO = 0.25
  JXA_FONT = "PingFangSC-Semibold"
  FONT_FILES = [
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc"
  ].freeze

  JXA = <<~JS
    ObjC.import('AppKit');
    function run(argv) {
      const text = $.NSString.alloc.initWithUTF8String(argv[0]);
      const size = parseFloat(argv[2]);
      let font = $.NSFont.fontWithNameSize(argv[1], size);
      if (!font || font.isNil()) font = $.NSFont.boldSystemFontOfSize(size);
      const attrs = $.NSDictionary.dictionaryWithObjectsForKeys(
        $([font, $.NSColor.whiteColor]), $([$.NSFontAttributeName, $.NSForegroundColorAttributeName]));
      const measured = text.sizeWithAttributes(attrs);
      const padX = Math.round(size * #{PAD_X_RATIO}), padY = Math.round(size * #{PAD_Y_RATIO});
      const w = Math.ceil(measured.width + padX * 2), h = Math.ceil(measured.height + padY * 2);
      const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(
        null, w, h, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
      $.NSGraphicsContext.saveGraphicsState;
      $.NSGraphicsContext.setCurrentContext($.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep));
      $.NSColor.colorWithCalibratedRedGreenBlueAlpha(0, 0, 0, #{BOX_ALPHA}).setFill;
      const corner = size * #{CORNER_RATIO};
      $.NSBezierPath.bezierPathWithRoundedRectXRadiusYRadius($.NSMakeRect(0, 0, w, h), corner, corner).fill;
      text.drawAtPointWithAttributes($.NSMakePoint(padX, padY), attrs);
      $.NSGraphicsContext.restoreGraphicsState;
      const png = rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $());
      if (!png.writeToFileAtomically(argv[3], true)) throw new Error('write failed');
      return 'ok';
    }
  JS

  module_function

  def font_pixels(height)
    [(height * FONT_HEIGHT_RATIO).round, MIN_FONT_PIXELS].max
  end

  # 渲染一张字幕 PNG，成功返回路径，所有渲染器都失败返回 nil。
  def render(text, height, output, env: ENV)
    text = text.to_s.strip
    return nil if text.empty?

    size = font_pixels(height)
    renderers = [-> { render_jxa(text, size, output, env) }, -> { render_magick(text, size, output, env) }]
    renderers.each do |renderer|
      File.delete(output) if File.exist?(output)
      return output if renderer.call && File.size?(output)
    end
    nil
  end

  # overlay 滤镜：base 是统一规格后的画面，image_input 是字幕 PNG 的输入序号。
  # 返回接在 filter_complex 里的两段，输出标签为 out。
  def filters(base, image_input, out, width, height, shot_seconds)
    shown = [[DISPLAY_SECONDS, shot_seconds].min, FADE_IN_SECONDS + FADE_OUT_SECONDS].max
    fade_out_at = format("%.3f", shown - FADE_OUT_SECONDS)
    x = (width * LEFT_RATIO).round
    y = (height * TOP_RATIO).round
    ["[#{image_input}:v]format=rgba,fade=t=in:st=0:d=#{FADE_IN_SECONDS}:alpha=1,fade=t=out:st=#{fade_out_at}:d=#{FADE_OUT_SECONDS}:alpha=1[cap]",
     "#{base}[cap]overlay=x=#{x}:y=#{y}:eof_action=pass:enable='lt(t,#{format('%.3f', shown)})'#{out}"]
  end

  # 字幕 PNG 作为输入：循环成与这一镜等长的画面流，淡入淡出才有时间轴可用。
  def input_args(path, total_seconds)
    ["-loop", "1", "-framerate", "30", "-t", format("%.3f", total_seconds), "-i", path]
  end

  def render_jxa(text, size, output, env)
    osascript = FFmpegTool.find("osascript", env: env) || (File.executable?("/usr/bin/osascript") ? "/usr/bin/osascript" : nil)
    return false unless osascript

    run([osascript, "-l", "JavaScript", "-e", JXA, text, JXA_FONT, size.to_s, output])
  end

  def render_magick(text, size, output, env)
    magick = FFmpegTool.find("magick", env: env) || FFmpegTool.find("convert", env: env)
    font = FONT_FILES.find { |path| File.file?(path) }
    return false unless magick && font

    pad_x = (size * PAD_X_RATIO).round
    pad_y = (size * PAD_Y_RATIO).round
    # 「label:@文件」会把文件内容读进来：开头的 @ 转义成字面字符。
    literal = text.start_with?("@") ? "\\#{text}" : text
    run([magick, "-background", "rgba(0,0,0,#{BOX_ALPHA})", "-fill", "white", "-font", font, "-pointsize", size.to_s,
         "label:#{literal}", "-bordercolor", "rgba(0,0,0,#{BOX_ALPHA})", "-border", "#{pad_x}x#{pad_y}", "PNG32:#{output}"])
  end

  def run(command)
    _out, err, status = Open3.capture3(*command)
    warn "video-studio: caption renderer #{File.basename(command.first)} failed: #{err.to_s.lines.last(2).join.strip}" unless status.success?
    status.success?
  rescue SystemCallError => error
    warn "video-studio: caption renderer #{File.basename(command.first)} unavailable (#{error.class})"
    false
  end
end
