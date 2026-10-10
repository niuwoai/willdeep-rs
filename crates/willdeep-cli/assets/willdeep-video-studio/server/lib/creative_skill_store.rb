# frozen_string_literal: true

require "fileutils"
require "json"
require "time"

# 创作技能的存储。
#
# 各阶段的系统提示词此前硬编码在前端字典里，用户改不了。搬成 markdown 之后
# 要解决一个矛盾：内置模板要能随插件更新，用户的修改又不能被下次安装覆盖。
#
# 做法是两层：插件包里的 `skills/creative/` 是模板，只读；用户编辑的副本放在
# 插件数据目录，首次访问时从模板复制过去。模板更新后不会自动覆盖已改过的
# 副本——那会悄悄丢掉用户的修改；界面上提供「恢复内置版本」让用户自己决定。
#
# 技能是全局一份，不是每部剧一份：改了对所有短剧生效。
class CreativeSkillStore
  Unavailable = Class.new(StandardError)

  # 阶段技能：每个阶段固定用哪一份，不由用户选。
  STAGE_SKILLS = {
    "planning" => "planning",
    "characters" => "character",
    "episodes" => "episodes-batch",
    "script" => "episode",
    "passage" => "passage",
    "storyboard" => "storyboard",
    "shot" => "storyboard",
    "frames" => "frames",
    # 资产阶段（造型、场景、道具、声音、参考包）共用一份资产技能。
    "appearance" => "assets",
    "scene" => "assets",
    "prop" => "assets",
    "voice" => "assets",
    "package" => "assets",
    "review" => "review",
    # 候选图质检（0.40.0-rc1，lib/image_qa.rb）：只用这一份，不拼合规红线（它查的是画面对不对，不是内容尺度）。
    "imageQA" => "image-qa",
    # 专家席审稿（0.42.0-rc1，lib/review_panel.rb）：一份技能里是整张专家席；由 ReviewPanel 自己解析，
    # 合规红线只给标了 [guard] 的那位专家，不经 compose 拼。
    "panel" => "panel"
  }.freeze

  # 合规红线：不由用户勾选，拼进每一个阶段。放在阶段技能之后、通用技能之前——
  # 通用技能（言情、穿越……）是题材偏好，底线要先于偏好出现。审核阶段同样带上，
  # 它就是审核员对照的那份标准。
  GUARD_SKILL = "compliance"

  MAX_BODY = 40_000
  # 模板目录里给宿主技能目录用的索引文件，不算技能。
  INDEX_FILE = "SKILL.md"

  def initialize(template_root:, user_root:)
    @template_root = File.expand_path(template_root)
    @user_root = File.expand_path(user_root)
  end

  attr_reader :template_root, :user_root

  # 全部技能的元信息，不带正文。界面用它画列表与勾选框。
  def list
    entries = (stage_ids + common_ids).map { |id| describe(id) }.compact
    { "skills" => entries }
  end

  # 一份技能的正文。没有用户副本时按模板返回，并顺手把副本落下来，
  # 这样用户第一次点开编辑就已经有文件可改。
  def get(id)
    key = sanitized(id)
    return nil unless known?(key)
    ensure_user_copy(key)
    body = read(user_path(key)) || read(template_path(key)) || ""
    describe(key)&.merge("body" => body)
  end

  def save(id, body)
    key = sanitized(id)
    raise ArgumentError, "unknown skill" unless known?(key)
    text = body.to_s
    raise ArgumentError, "skill body too long" if text.length > MAX_BODY
    write(user_path(key), text)
    get(key)
  end

  # 丢掉用户副本，回到内置模板。刻意做成显式操作：模板更新时静默覆盖
  # 用户改过的文件，等于替他把修改删了。
  def reset(id)
    key = sanitized(id)
    raise ArgumentError, "unknown skill" unless known?(key)
    path = user_path(key)
    File.delete(path) if File.exist?(path)
    get(key)
  end

  # 拼给模型的 system 文本：阶段技能在前，勾选的通用技能按给定顺序追加。
  def compose(stage:, common: [])
    stage_id = STAGE_SKILLS[stage.to_s]
    parts = []
    if stage_id && (entry = get(stage_id))
      parts << entry["body"]
    end
    if stage_id && (guard = get(GUARD_SKILL))
      parts << "\n\n---\n\n" + guard["body"].to_s
    end
    # 审核只对照红线，不叠题材偏好：言情技能里「拉扯要到位」之类的话会把
    # 审核员带偏。
    Array(stage.to_s == "review" ? [] : common).each do |raw|
      key = sanitized(raw)
      next unless common_ids.include?(key)
      entry = get(key)
      next unless entry
      parts << "\n\n---\n\n" + entry["body"].to_s
    end
    { "ok" => true, "system" => parts.join.strip, "stage" => stage.to_s }
  end

  private

  def stage_ids
    @stage_ids ||= discover(@template_root, common: false)
  end

  def common_ids
    @common_ids ||= discover(File.join(@template_root, "common"), common: true)
  end

  # 模板目录是唯一的技能清单来源：用户不能凭空造出一份新技能，否则
  # 阶段与技能的对应关系就没人保证了。
  def discover(dir, common:)
    return [] unless File.directory?(dir)
    # SKILL.md 是给宿主技能目录看的索引（0.41.0-rc3），不是一份阶段技能。
    Dir.children(dir).select { |name| name.end_with?(".md") && name != INDEX_FILE }.map do |name|
      id = File.basename(name, ".md")
      common ? "common/#{id}" : id
    end.sort
  end

  def known?(id)
    stage_ids.include?(id) || common_ids.include?(id)
  end

  def describe(id)
    body = read(user_path(id)) || read(template_path(id))
    return nil unless body
    meta = frontmatter(body)
    {
      "id" => id,
      "kind" => id.start_with?("common/") ? "common" : (id == GUARD_SKILL ? "guard" : "stage"),
      "name" => meta["name"] || id,
      "description" => meta["description"] || "",
      "stage" => meta["stage"],
      "customized" => customized?(id),
      "updatedAt" => File.exist?(user_path(id)) ? File.mtime(user_path(id)).utc.iso8601 : nil
    }
  end

  # 只取 name / description / stage 三个键。这里刻意不引 YAML：技能是
  # 用户可编辑的文本，一个手写错的缩进不该让整个列表读不出来。
  def frontmatter(body)
    lines = body.split("\n")
    return {} unless lines.first&.strip == "---"
    meta = {}
    lines.drop(1).each do |line|
      break if line.strip == "---"
      key, _, value = line.partition(":")
      next if value.empty?
      meta[key.strip] = value.strip
    end
    meta
  end

  # 「已定制」按内容判，不按副本是否存在判。
  #
  # 副本在首次读取时就会被建出来（好让用户点开编辑时已经有文件可改），所以
  # 用存在性判会让每一份技能一读就显示成已修改；reset 之后紧接着的那次读取
  # 也会立刻把它标回已修改。顺带一个好处：用户手动改回原样时状态会自己复位。
  def customized?(id)
    user = read(user_path(id))
    return false unless user
    template = read(template_path(id))
    return true unless template
    user != template
  end

  def sanitized(id)
    token = id.to_s.strip
    return "" if token.empty?
    # 只允许 `name` 与 `common/name` 两种形状，挡住 `../` 之类。
    return "" unless token =~ %r{\A(common/)?[A-Za-z0-9._-]+\z}
    token
  end

  # 端点方法（`def f(x) = expr`）是 Ruby 3 的语法，而 mcp.json 把解释器钉死在
  # macOS 自带的 /usr/bin/ruby 2.6 上，那里会直接语法错误。
  def template_path(id)
    File.join(@template_root, "#{id}.md")
  end

  def user_path(id)
    File.join(@user_root, "#{id}.md")
  end

  def ensure_user_copy(id)
    path = user_path(id)
    return if File.exist?(path)
    template = read(template_path(id))
    return unless template
    write(path, template)
  end

  def read(path)
    File.read(path, encoding: "UTF-8")
  rescue Errno::ENOENT
    nil
  rescue EncodingError, SystemCallError, IOError => error
    raise Unavailable, "creative skill is unreadable (#{error.class})"
  end

  def write(path, text)
    FileUtils.mkdir_p(File.dirname(path))
    # 不在文件锁里写：几条画面质检并发时（0.38.0-rc2 逐镜流水线）会同时第一次复制审核技能，
    # 临时文件名只带进程号时，一个线程 rename 走了另一个线程的临时文件，后者报 ENOENT。
    temporary = "#{path}.#{Process.pid}.#{Thread.current.object_id}.tmp"
    File.write(temporary, text, perm: 0o600, encoding: "UTF-8")
    File.rename(temporary, path)
    temporary = nil
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
