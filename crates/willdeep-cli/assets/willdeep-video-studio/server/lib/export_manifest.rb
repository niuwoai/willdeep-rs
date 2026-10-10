# frozen_string_literal: true

require_relative "media_ref"
require_relative "reference_package"

# `drama.export_manifest`：把一部剧交给 harness 或剪辑工具的交接清单（设计稿第 0 节）。
#
# 插件的 Ruby 2.6 不拼片，也不该拼；但自动化生产的终点是一集成片，做这一步的人或
# 程序需要知道：每一镜按顺序的选定首尾帧、成片文件、对白音频与时长、当时发出去的
# 参考快照、审核状态、有没有用合成声音。全部给绝对路径，调用方不必再猜媒体目录。
module ExportManifest
  module_function

  def build(drama, jobs, media_root)
    references = {}
    assets = Array(drama["assets"])
    warnings = []
    episodes = Array(drama["episodes"]).sort_by { |episode| episode["order"].to_i }.map do |episode|
      shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }.map do |shot|
        video = latest_job(jobs, shot)
        start = MediaRef.selected({ "candidates" => shot["startCandidates"], "selectedCandidateID" => shot["selectedStartID"] }, media_root)
        finish = MediaRef.selected({ "candidates" => shot["endCandidates"], "selectedCandidateID" => shot["selectedEndID"] }, media_root)
        dialogue = Array(shot["dialogue"]).map do |line|
          next nil unless line.is_a?(Hash)
          audio = line["audio"].is_a?(Hash) ? MediaRef.selected(line["audio"], media_root) : nil
          { "id" => line["id"], "speaker" => line["speaker"], "text" => line["text"],
            "audio" => audio && { "fileName" => audio["fileName"], "filePath" => audio["filePath"], "durationMs" => audio["durationMs"], "voiceAssetID" => audio["voiceAssetID"] } }
        end.compact
        synthetic = dialogue.any? { |line| line["audio"] }
        ReferencePackage.referenced_asset_ids(shot).each { |id| references[id] = true }
        warnings << { "code" => "missing_video", "episodeID" => episode["id"], "shotID" => shot["id"], "message" => "第 #{episode['order']} 集第 #{shot['order']} 镜还没有完成的成片。" } unless video && video["state"] == "completed"
        {
          "id" => shot["id"], "order" => shot["order"], "title" => shot["title"], "summary" => shot["summary"],
          "duration" => shot["duration"], "cameraIntent" => shot["cameraIntent"],
          "startFrame" => start && media_entry(start), "endFrame" => finish && media_entry(finish),
          "dialogue" => dialogue, "syntheticVoice" => synthetic,
          "package" => shot["package"],
          "video" => video && {
            "jobID" => video["id"], "state" => video["state"], "outputPath" => video["outputPath"], "mediaPath" => video["mediaPath"],
            "generationMode" => video["generationMode"] || video["mode"], "referenceSnapshot" => video["referenceSnapshot"] || [],
            "review" => video["review"], "createdAt" => video["createdAt"]
          },
          "reviews" => shot["reviews"].is_a?(Hash) ? shot["reviews"] : {}
        }
      end
      { "id" => episode["id"], "order" => episode["order"], "title" => episode["title"], "summary" => episode["summary"],
        "reviews" => episode["reviews"].is_a?(Hash) ? episode["reviews"] : {}, "shots" => shots }
    end

    {
      "ok" => true,
      "drama" => { "id" => drama["id"], "title" => drama["title"], "genre" => drama["genre"], "format" => drama["format"],
                   "episodeCount" => episodes.length, "reviews" => drama["reviews"].is_a?(Hash) ? drama["reviews"] : {} },
      "mediaRoot" => File.expand_path(media_root.to_s),
      "characters" => Array(drama["characters"]).map do |character|
        selected = MediaRef.selected(character, media_root)
        { "id" => character["id"], "name" => character["name"], "identityImage" => selected && media_entry(selected) }
      end,
      "assets" => assets.select { |asset| references[asset["id"]] || !asset["archived"] }.map do |asset|
        selected = MediaRef.selected(asset, media_root)
        entry = { "id" => asset["id"], "kind" => asset["kind"], "name" => asset["name"], "archived" => asset["archived"] == true,
                  "referenced" => references[asset["id"]] == true, "selected" => selected && media_entry(selected) }
        entry["consent"] = asset["consent"] if asset["kind"] == "voice"
        entry
      end,
      "episodes" => episodes,
      "syntheticVoiceNotice" => episodes.any? { |episode| episode["shots"].any? { |shot| shot["syntheticVoice"] } },
      "warnings" => warnings,
      "exportedAt" => Time.now.utc.iso8601
    }
  end

  def media_entry(media)
    { "id" => media["id"], "fileName" => media["fileName"], "filePath" => media["filePath"], "durationMs" => media["durationMs"] }
  end

  # 先按 shotID，再按参考图路径反查旧任务。交接清单要的是能用的成片：同一镜有多次
  # 提交时优先最新一次**已完成**的，一次都没完成才给最新的那次（让调用方看到状态）。
  def latest_job(jobs, shot)
    linked = Array(jobs).select { |job| job["shotID"] == shot["id"] }
    if linked.empty?
      start = Array(shot["startCandidates"]).find { |entry| entry["id"] == shot["selectedStartID"] }
      return nil unless start && !start["filePath"].to_s.empty?
      linked = Array(jobs).select { |job| job["shotID"].to_s.empty? && job["referenceImagePath"] == start["filePath"] }
    end
    completed = linked.select { |job| job["state"] == "completed" && !job["outputPath"].to_s.empty? }
    (completed.empty? ? linked : completed).max_by { |job| job["createdAt"].to_s }
  end
end
