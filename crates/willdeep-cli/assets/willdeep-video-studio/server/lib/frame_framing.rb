# frozen_string_literal: true

# 首尾帧的取景句（0.41.0-rc1）：把分镜运镜（cameraIntent）里的景别与机位翻成出图模型看得懂的一句话，
# 放在参考图说明之后、首尾帧描述之前。
#
# 起因（《回村养鸭》第 5～8 集）：首帧描述多半只写人和事，景别只在运镜里（「中景，固定，过许禾肩」
# 「近景，低机位与鸭子平视」）。出图请求只带描述，模型默认画成人物从头到脚的全景，图片质检对照运镜判
# composition 不符——四集兜底选用的被拦首帧里，33 条 must 级问题有一大半是景别。
#
# 规矩：
# - 首帧取运镜里第一个景别词，尾帧取最后一个（「中景开场，推到秤杆刻度特写」首帧中景、尾帧特写）。
# - 景别词按长的先认：大特写、中近景、中全景先于特写、近景、全景。
# - 每个景别写成「人物哪儿以上入画」这种画面上看得见的正面说法；机位（俯拍、仰拍、低机位、过肩、侧面）跟在后面。
# - 运镜里没有景别也没有机位时返回空串，提示词不变。
module FrameFraming
  module_function

  SCALE = /大特写|中近景|中全景|特写|近景|中景|全景|远景/.freeze
  SCALE_TEXT = {
    "大特写" => "大特写，一处局部细节占满画面",
    "特写" => "特写，主体（脸、手或物件）占满画面",
    "近景" => "近景，人物胸部以上入画，脸部清晰",
    "中近景" => "中近景，人物腰部以上入画",
    "中景" => "中景，人物膝盖以上入画",
    "中全景" => "中全景，人物大半身入画，带少量环境",
    "全景" => "全景，人物全身入画，带周围环境",
    "远景" => "远景，环境为主，人物在画面中较小"
  }.freeze
  ANGLES = [
    [/俯拍|俯视|顶拍/, "镜头从高处向下俯拍"],
    [/仰拍|仰视/, "镜头从低处向上仰拍"],
    [/低机位/, "低机位，镜头贴近地面"],
    [/过(\p{Han}{1,4}?)肩/, ->(match) { "过肩构图，前景是#{match[1]}的肩膀与后脑" }],
    [/侧面/, "侧面构图"]
  ].freeze

  # kind：start / end。返回一句「镜头取景：…。」或空串。
  def line(camera_intent, kind)
    text = camera_intent.to_s
    scales = text.scan(SCALE)
    scale = kind.to_s == "end" ? scales.last : scales.first
    parts = []
    parts << SCALE_TEXT.fetch(scale) if scale
    ANGLES.each do |pattern, phrase|
      match = text.match(pattern)
      next unless match

      parts << (phrase.respond_to?(:call) ? phrase.call(match) : phrase)
    end
    parts.empty? ? "" : "镜头取景：#{parts.join('，')}。"
  end
end
