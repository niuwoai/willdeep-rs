# frozen_string_literal: true

require 'digest'
require 'find'

# 一棵目录树的内容哈希：相对路径与文件内容都算，改名、改内容、增删文件都会变。
# 任务集整体（`dataset_sha256`）和单个任务（`Task#content_sha256`）用同一个算法。
module TreeDigest
  module_function

  def sha256(root)
    digest = Digest::SHA256.new
    files = []
    Find.find(root) { |path| files << path if File.file?(path) }
    files.sort.each do |path|
      digest.update(path.delete_prefix(root))
      digest.update("\0")
      digest.update(File.binread(path))
      digest.update("\0")
    end
    digest.hexdigest
  end
end
