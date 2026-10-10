# frozen_string_literal: true

require "json"

# 画幅表。首尾帧的图和随后那段视频必须是同一个比例，否则上游会自己裁切或
# 拉伸首帧——人物位置、景别和留白当场走样，而且这件事发生在扣完费之后。
#
# 表本身是 schemas/aspect-ratios-v1.json，页面 import 的是同一份文件：一边改
# 比例另一边不知道，正是这条 bug 本身的形状，不能靠两处手写同步。
module AspectRatios
  # 显式钉死编码：LANG 缺失时默认外部编码是 US-ASCII，表里的中文注释会让
  # JSON.parse 直接抛编码异常（理由同 DramaStore#write）。
  PATH = File.expand_path("../../schemas/aspect-ratios-v1.json", __dir__)
  CATALOG = JSON.parse(File.read(PATH, encoding: "UTF-8")).freeze
  RATIOS = CATALOG.fetch("ratios").map(&:freeze).freeze
  IDS = RATIOS.map { |entry| entry.fetch("id") }.freeze
  DEFAULT_ID = CATALOG.fetch("default")

  # 0.30.0-rc3 之前「9:16」的真实画幅：出图写死 1024x1536、成片 768x1152，都是 2:3。
  # 旧剧留在这一档，已经画好的首尾帧和成片才不会一次升级就全对不上。
  LEGACY_ID = CATALOG.fetch("legacy")
  # 旧剧里哪一个画幅值其实是 LEGACY_ID。16:9 / 1:1 / 4:5 的旧剧首尾帧本来就是
  # 竖 2:3、和成片对不上，直接改用新表，不在此列。
  LEGACY_SOURCE_ID = "9:16"

  # 用户与 Agent 能选的画幅。legacy 只给旧剧用，不开放选择。
  SELECTABLE_IDS = (IDS - [LEGACY_ID]).freeze

  # 剧上的 `aspectSpec` 标记：等于它说明 aspectRatio 按这张表的真实比例解释。
  # 新建的剧、以及用 drama.save_metadata 显式设过画幅的剧会写上它；没有它的
  # 旧剧 9:16 仍按 LEGACY_ID 出图出片。
  SPEC_VERSION = 2

  # 求一部剧的有效画幅 id。页面与 Agent 都读 DramaService 附上的 `frame`，
  # 这里是唯一一处判断「旧剧 9:16 = 2:3」的地方。
  def self.effective_id(aspect_ratio, aspect_spec)
    id = normalize(aspect_ratio)
    return LEGACY_ID if id == LEGACY_SOURCE_ID && !current_spec?(aspect_spec)

    id
  end

  def self.current_spec?(aspect_spec)
    aspect_spec.is_a?(Integer) && aspect_spec >= SPEC_VERSION
  end

  def self.find(id)
    RATIOS.find { |entry| entry["id"] == id.to_s }
  end

  # 认不出来的回落到 fallback，不抛错：读不出画幅不该让整部剧打不开。
  def self.normalize(id, fallback: DEFAULT_ID)
    find(id) ? id.to_s : fallback
  end

  def self.entry(id)
    find(normalize(id))
  end

  # 发给出图的 "WxH"。
  def self.image_size(id)
    entry(id).fetch("image")
  end

  # 发给视频上游的 { "width", "height" }，拷一份出去，调用方改了也不会污染表。
  def self.video_size(id)
    entry(id).fetch("video").dup
  end

  # 能出这一档的出图模型。gpt-image 系只有三种尺寸，出不了 9:16 / 16:9 / 4:5。
  def self.image_models(id)
    entry(id).fetch("imageModels").dup
  end

  def self.supports?(id, model)
    image_models(id).include?(model.to_s)
  end
end
