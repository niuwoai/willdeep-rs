# frozen_string_literal: true

require_relative '../model_eval/report'

module PromptRsi
  module Preflight
    module_function

    def check!(root:, binary_version:, binary_commit:)
      context = ModelEval::Report.git_context(root)
      raise 'RSI preflight：工作区有未提交的实现或任务改动，停止付费评测' if context[:dirty]
      raise 'RSI preflight：无法确定源码 commit' unless context[:commit]
      raise 'RSI preflight：二进制构建 commit 与源码不一致' unless binary_commit&.start_with?(context[:commit]) && !binary_commit.end_with?('-dirty')

      version = File.read(File.join(root, 'Cargo.toml'))[/^version\s*=\s*"([^"]+)"/, 1]
      raise 'RSI preflight：二进制与源码版本不一致' unless binary_version == version

      context
    end
  end
end
