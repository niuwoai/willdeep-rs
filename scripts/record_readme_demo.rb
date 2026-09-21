#!/usr/bin/env ruby
# frozen_string_literal: true

# 录 README 首屏的 TUI 演示 GIF（docs/media/readme-demo.gif）。
#
# 真跑模型：从 ~/.willdeep/config.toml 取默认 provider 的端点与 key，key 只经
# 环境变量传给 vhs 里的 willdeep，不写进任何文件。演示仓库与 WILLDEEP_HOME 都在
# /tmp/willdeep-demo 下现建现删——画面里不能出现真实用户名、私有仓库或 key。
#
#   cargo build --release -p willdeep && ruby scripts/record_readme_demo.rb
#
# 需要 vhs（brew install vhs）。

require 'fileutils'
require_relative 'lib/willdeep_credentials'

REPO_ROOT = File.expand_path('..', __dir__)
DEMO_ROOT = '/tmp/willdeep-demo'
SHOP = File.join(DEMO_ROOT, 'shop')
HOME_DIR = File.join(DEMO_ROOT, 'home')
MODEL = ENV['WILLDEEP_DEMO_MODEL'] || 'deepseek-v4-flash'
BINARY = File.join(REPO_ROOT, 'target', 'release', 'willdeep')
GIF = File.join(REPO_ROOT, 'docs', 'media', 'readme-demo.gif')
MAX_GIF_BYTES = 3 * 1024 * 1024
FRAMES = File.join(DEMO_ROOT, 'frames')
# vhs 的截帧率；帧目录里 text / cursor 两层各一张。
CAPTURE_FPS = 50
MAX_SECONDS = 24.0
OUTPUT_FPS = 5
ATTEMPTS = (ENV["WILLDEEP_DEMO_ATTEMPTS"] || 3).to_i
OUTPUT_WIDTH = 800
PALETTE_COLORS = 24

CALC = <<~PY
  def apply_discount(price, percent):
      """price 打 percent 折扣后的价格，percent 取 0–100。"""
      return price - price * percent


  def total(items):
      return sum(price * qty for price, qty in items)
PY

TESTS = <<~PY
  import unittest

  from calc import apply_discount, total


  class CalcTest(unittest.TestCase):
      def test_discount_is_a_percentage(self):
          self.assertEqual(apply_discount(200, 10), 180)

      def test_total(self):
          self.assertEqual(total([(10, 2), (5, 1)]), 25)


  if __name__ == "__main__":
      unittest.main()
PY

abort "先构建：cargo build --release -p willdeep（找不到 #{BINARY}）" unless File.executable?(BINARY)
abort '需要 vhs：brew install vhs' unless system('which vhs', out: File::NULL)

# 只清理这个脚本自己建的目录。
FileUtils.rm_rf(DEMO_ROOT)
FileUtils.mkdir_p([SHOP, HOME_DIR])
File.write(File.join(SHOP, 'calc.py'), CALC)
File.write(File.join(SHOP, 'test_calc.py'), TESTS)
# 真实 Python 仓库都会忽略字节码缓存；不忽略的话第一次跑测试生成的 .pyc 会让
# 验证快照变化，那次通过被判无效。
File.write(File.join(SHOP, '.gitignore'), "__pycache__/\n")
git = ->(*args) { system('git', '-C', SHOP, '-c', 'user.name=demo', '-c', 'user.email=demo@example.com', *args, out: File::NULL, exception: true) }
git.call('init', '-q', '-b', 'main')
git.call('add', '.')
git.call('commit', '-q', '-m', 'init')

_provider, api_base, api_key = WilldeepCredentials.default_provider(
  File.join(Dir.home, '.willdeep', 'config.toml')
)
File.write(File.join(HOME_DIR, 'config.toml'), <<~TOML)
  version = 1
  default_provider = "demo"

  [agent]
  approval = "workspace-write"
  language = "zh-CN"
  input_suggestions = true
  # 演示只看主 Agent 一轮：不派子 Agent，画面更短、更干净。
  small_model_routing = false
  auto_dispatch_read_only = false

  [providers.demo]
  provider = "some-im"
  api = "chat-completions"
  api_base = "#{api_base}"
  api_key_env = "SOMEIM_API_KEY"
  model = "#{MODEL}"
TOML

recorded = false
env = {
  'WILLDEEP_HOME' => HOME_DIR,
  'SOMEIM_API_KEY' => api_key,
  'PATH' => "#{File.dirname(BINARY)}:#{ENV.fetch('PATH')}"
}
begin
  # 模型每次说的话不一样：收尾时判断「没有下一步」就不出灰字预测，这一条录不成。
  # 重录最多 ATTEMPTS 次，每次先清掉上一轮的帧与 daemon。
  frames = 0
  ATTEMPTS.times do |attempt|
    FileUtils.rm_rf(FRAMES)
    system(env, BINARY, 'daemon', 'stop', out: File::NULL, err: File::NULL)
    git.call('checkout', '-q', '--', '.')
    ok = system(env, 'vhs', File.join(REPO_ROOT, 'docs', 'media', 'readme-demo.tape'), chdir: REPO_ROOT)
    frames = Dir.glob(File.join(FRAMES, 'frame-text-*.png')).size
    break if ok && frames.positive?

    warn "第 #{attempt + 1} 次没录到灰字预测，重录"
    frames = 0
  end
  abort "#{ATTEMPTS} 次都没录成，见上面的输出" unless frames.positive?

  # 真实时长超过 MAX_SECONDS 就整体加速；等模型的那段本来就是看工具行滚动，快放不丢信息。
  seconds = frames.to_f / CAPTURE_FPS
  speed = [seconds / MAX_SECONDS, 1.0].max
  # 3 MB 上限的大头是聊天区滚动：几乎每帧都变。低帧率、少颜色、缩到 README 的显示宽度。
  filter = "[0][1]overlay,setpts=PTS/#{speed.round(3)},fps=#{OUTPUT_FPS}," \
           "scale=#{OUTPUT_WIDTH}:-2:flags=area,split[a][b];" \
           "[a]palettegen=max_colors=#{PALETTE_COLORS}:stats_mode=diff[p];" \
           '[b][p]paletteuse=dither=none:diff_mode=rectangle'
  system('ffmpeg', '-y', '-loglevel', 'error',
         '-framerate', CAPTURE_FPS.to_s, '-i', File.join(FRAMES, 'frame-text-%05d.png'),
         '-framerate', CAPTURE_FPS.to_s, '-i', File.join(FRAMES, 'frame-cursor-%05d.png'),
         '-filter_complex', filter, GIF, exception: true)
  puts "录制 #{seconds.round(1)} 秒，#{speed.round(2)} 倍速 → #{(seconds / speed).round(1)} 秒"
  recorded = true
ensure
  system(env, BINARY, 'daemon', 'stop', out: File::NULL, err: File::NULL)
  # 失败时留现场（帧、会话、daemon 日志）方便排查；WILLDEEP_DEMO_KEEP=1 也保留。
  if recorded && ENV['WILLDEEP_DEMO_KEEP'].to_s.empty?
    FileUtils.rm_rf(DEMO_ROOT)
  else
    warn "现场保留在 #{DEMO_ROOT}，看完手动删除"
  end
end

size = File.size(GIF)
puts "#{GIF}: #{(size / 1024.0 / 1024).round(2)} MB"
abort "GIF 超过 3 MB（#{size} 字节）：调低 OUTPUT_FPS / PALETTE_COLORS / OUTPUT_WIDTH" if size > MAX_GIF_BYTES
