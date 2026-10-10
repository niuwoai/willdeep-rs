---

name: video-studio
description: Drive the Short Drama Studio MCP server end to end — plan a short drama, lock character identities, build reusable assets (appearances, scenes, props, voices), write episodes and storyboard shots, bind reference packages, generate frames, dialogue audio and asynchronous AI videos, review content, and export a hand-off manifest. Use when the user asks to plan, write, storyboard, shoot, voice, review, inspect or export a short drama through Video Studio, from any MCP client.
---

# Short Drama Studio

用户明确授权在制作中自动修复并安装插件时，同时读取 `../production-repair/SKILL.md`。该流程用于工程缺陷；正常创作仍走本技能下的制作工具。更新后先用只读 `system.status` 验证运行版本与进程归属。

主框架或某一集需要自动创作与审改时，使用 `drama.write_with_panel`（scope 为 drama 或 episode，已有短剧 ID / 集 ID）。空正文先写初稿，已有正文先审稿；后台任务按必改项修订并复审，默认最多两次修订，可用 maxRevisions 设为 0～3。用 jobs.wait / jobs.get 查看结果，jobs.cancel 停止后续步骤。state=needs_human 时保留最新稿及审核意见，明确交接未解决项，不重复启动来绕过上限。此工具只改文字，不替代确认策划、创建角色、分镜和资产的工具。

Every capability lives in the `video-studio` MCP server. You never need another
tool for short-drama work: state, materials, image / audio / video generation,
review material, progress and export all come from `drama.*`, `image.generate`,
`voice.generate`, `video.*`, `review.*` and `media.read`. The only thing the
server can also delegate prose and panel revisions through `drama.write_with_panel`
when the host provides AI completion; direct writing uses the stage brief and save tools.

Do not use any host-provided short-drama, image or video tools for this drama;
the results would not land in the plugin's data and the user would not see them.

## Working loop

**Start every session with `drama.get_progress`** and call it again after each
batch. For a long drama it lists each episode as a summary; pass `episodeOrder`
for the episode you are working on to get its shots and its own next steps. `nextSteps` is already in production order with the tool and target IDs
(pass `maxSteps` if the list is cut off). It also carries per-shot `continuity`
issues; fix those before spending on frames or video.

**Before writing anything, call `drama.get_stage_context`** for that stage and
target. Treat its `system` text as your instructions (it includes the content
compliance guard, which is not optional), use its `context` as material, and
write back with the tool named in `write`. If `target.pendingDraft` is not
null the user has unfinished edits there: ask before overwriting.

**Long work runs in the background.** Prefer the episode batch tools —
`episode.generate_frames`, `episode.dub`, `episode.generate_videos`,
`review.run_batch` — over looping single calls shot by shot; `nextSteps`
entries carry `batch {tool, arguments}` and `batchSteps` sums them up per
episode (with `runningJobID` when that batch is already running — do not
submit it again). They return a `jobID` at once; follow with `jobs.wait
jobIDs timeoutSeconds` (returns early when done; it caps the wait itself, so
just call it again while `timedOut`) or `jobs.status`, which shows per-item
results (`items[]` with state, reason and error). Single `image.generate`,
`review.run` and `voice.generate` calls take `async: true` too. Reads such as
`drama.get` answer while jobs run. A job reported `interrupted` was cut off by
a plugin restart: call the same tool again — finished items are skipped and
derived requestIDs keep anything from being billed twice. `jobs.cancel` stops
a job before its next item.

Every `save_*` tool is a partial update: send only the fields you change; never
send an empty string for a field you meant to leave alone. On
`revision_conflict`, re-read and write again — the user may be editing in the
page.

## Driving it from the WillDeep chat

The same tools are reachable from WillDeep's own chat (macOS app, `willdeep`
CLI and its web app). There they are named `mcp__video-studio__<tool>` (the
server name keeps its hyphen) with dots in the tool name turned into
underscores (`drama.get` → `mcp__video-studio__drama_get`). Copy the exact name
from the tool list.
If they are not in the tool list yet, call `list_mcp_tools` with query
`video` once. When the host offers a tool with a single `arguments_json`
parameter, pass the tool's arguments as one JSON object string, e.g.
`{"dramaID":"…","episodeID":"…","target":"start","dryRun":true}`; the
argument names are the ones in this skill and in `tools/list`.

Use `async: true` or the episode batch tools for anything that generates, and
poll with `jobs.wait` / `jobs.status`: a single call that keeps the plugin busy
for minutes also blocks the plugin page.

**Show the media you made, for review in the chat.** After frames, voice lines,
clips or an episode cut are ready, put each file the user should look at on a
line of its own as a Markdown image or link with its absolute path in angle
brackets (paths often contain spaces), taken from
the tool result (`generated[].filePath` of `image.generate`, the line audio
`filePath` of `voice.generate`, a video job's `outputPath`, the compose job's
`outputPath`):

```markdown
![第 3 镜首帧 · 候选 1](</Users/…/plugin-data/willdeep-video-studio/generated-images/willdeep-generated-image-….png>)
![第 3 集成片](</Users/…/Movies/WillDeep Video Studio/…/第03集.mp4>)
[许禾 · 第 2 句](</Users/…/plugin-data/willdeep-video-studio/generated-images/voice-….wav>)
```

The macOS app renders those lines as an image, a video player and an audio
player; other clients show links. Keep it to the files the user should judge
now (about eight per message): the selected or recommended candidate, the clip
that QA flagged, the finished cut. Say in one line what to look for (identity,
wardrobe, jump cut, speech pace) so the review is quick.

## Pipeline

1. **Plan.** Talk the plan through with the user. Call `drama.confirm_plan`
   only after they confirm the planning card; pass a stable `requestID`.
   Write `tone` and `visualStyle` separately: `tone` is guidance for the
   writers (how people talk, pacing, what not to write) and never reaches the
   image or video models; `visualStyle` is one visual-only sentence (light,
   colour, texture, lens) that goes verbatim into every video prompt — no
   story, dialogue or rules, or the video model may burn it in as subtitles.
   Older dramas may lack it: set it with `drama.save_draft scope=drama
   commit=true` when `video.generate dryRun` warns `visual_style_missing`.
   Then record the **series canon** with `drama.save_canon` (see "Series
   canon" below): the numbers that evolve, story props, visual rules,
   banned terms and functional speakers.
2. **Characters.** `drama.get_stage_context stage=characters`, then
   `drama.save_draft scope=character commit=true` (name, description,
   visualPrompt). `image.generate target=character` draws identity
   candidates; look at them with `media.read` and pick one with
   `drama.select_character_image`, saying why.
3. **Assets** (optional but what keeps a series consistent):
   - Appearances: `stage=appearance characterID=…` →
     `drama.save_asset kind=appearance characterID category` (wardrobe, hair,
     makeup, age, state). Revise text with `drama.save_draft scope=asset
     assetID commit=true`. `image.generate target=appearance assetID` attaches
     the character's identity image automatically; select with
     `drama.select_asset_media`.
   - Scenes: `kind=scene` (optionally `parentSceneID`), variants
     `kind=sceneVariant sceneID lighting`. Notes are continuity rules and are
     inherited down the scene tree.
   - Props: `kind=prop`.
   - Voices: `kind=voice characterID language referenceTranscript
     providerVoiceID presets`. **Consent is a hard gate**: `voice.generate`
     refuses a voice until the user confirms `consent {status: granted,
     grantedBy}`. Never set consent on the user's behalf. Preview a voice
     before dubbing: a voice whose clips average below 2.5 or above 6.5
     Chinese characters per second gets `speech_rate_slow` /
     `speech_rate_fast` (on `voice.generate`, `drama.list_assets` and
     `episode.compose_plan`); a slow storyteller voice on a brisk character
     sounds eerie — switch `providerVoiceID` or the preset speed.
   `drama.list_assets` lists them with reference counts; assets are archived
   with `drama.archive_asset`, never deleted.
4. **Episodes and shots.** `drama.save_episode_drafts commit=true` /
   `drama.save_draft scope=episode`, then `drama.save_shot_drafts commit=true`
   (dialogue is `[{speaker, text}]`, duration 4 to 15 seconds; `soundscape`
   is ambient and action sound in English without dialogue, `music` is
   off-screen score or `N/A`). After each batch, `drama.check_consistency`
   and fix what it lists before storyboarding or drawing frames.
5. **Reference package per shot.** `stage=package` gives the catalogue;
   `drama.set_reference_package` binds scene (+variant), cast (each with
   appearance, voice, role, screenPosition, `appearanceChange` when the look
   differs from the previous shot) and props. Bind only what appears in the
   shot: more references confuse the model. `drama.preview_reference_package
   purpose=image|video` shows exactly what will be sent, what was dropped and
   why, and the resolved mode.
6. **Frames.** `stage=frames` → `drama.save_draft scope=shot commit=true`
   (startPrompt / endPrompt). `image.generate target=start dryRun=true` first,
   then the real call; `drama.select_image kind=start`. The prompt starts with
   a reference legend naming each attached image (who, age/gender, which
   look, which scene); check it in dryRun. Candidates flagged
   `aspectMismatch` (counted in `aspectMismatchCount`) came back in the wrong
   orientation — regenerate instead of selecting them. For a whole episode:
   `episode.generate_frames dramaID episodeID target=start countPerModel`
   (`shotOrders` for some shots; `onlyMissing` skips shots that already have
   candidates, `force: true` redraws). It stops when the image quota runs out.
   With candidate image QA on (see "Candidate image QA" below) every new
   candidate is checked and the batch selects the recommended frame itself;
   read each item's `selectionReason` and handle the `needs_human` shots.
7. **Dialogue audio** (when a TTS backend is configured): `voice.generate
   episodeID shotID` speaks every line whose speaker has a consented voice;
   `episode.dub dramaID episodeID` does the whole episode, skipping lines whose
   audio matches the current text and redoing (and selecting) stale ones;
   `drama.select_dialogue_audio` if you generate alternatives. Progress
   reports `audio_stale` when a line changed and `audio_too_long` when audio
   exceeds the shot duration.
8. **Video.** `video.capabilities` tells you which modes the provider supports.
   `video.generate dramaID episodeID shotID dryRun=true` shows the resolved
   mode, the reference snapshot and the structured prompt that will be sent;
   the real call links the job to the shot. Mode `auto` picks `ref2va` with
   the bound reference videos (`package.videoRefs`, up to 3 completed clips),
   else `ref2va` with the start frame plus the shot's dialogue track (lines
   concatenated; the mouths follow your TTS audio), else `fl2va`, else `t2va`.
   The prompt is rendered from the package, the shot text, `soundscape` and
   `music`; pass a plain sentence to add it to `[Shot 1]`, or a full prompt
   with the section names to replace it. Poll with `video.refresh` /
   `video.refresh_active`; a submitted job is not a finished video.
   `episode.generate_videos dramaID episodeID` submits every shot that has a
   selected start frame (or all shots with `mode=t2va|ref2va`), then polls and
   downloads in the background; shots with a finished clip are skipped and
   ones still generating are polled, not resubmitted. Each submission is
   billed: tell the user how many shots first (`planned.pending`). With
   `video.settings` `autoQA` and `autoRemediate` on (the defaults) and the
   WillDeep model bridge available, the batch reviews each clip's frames as
   soon as that clip is ready and remediates it (see "Clip QA and
   remediation"), so a shot can cost up to 1 + `remediateMaxRetries` clips:
   say so when you report the count. Shots run concurrently (at most
   `video.settings` `videoConcurrency` clips generating and `qaConcurrency`
   reviews at once, across the plugin); follow them with `jobs.wait` — each
   item's `stage` says generating / qa / retake (`attempt`) / selecting /
   done / needs_human. If the job ends `interrupted`, call the same tool
   again: unfinished QA and retake chains continue without resubmitting.
9. **Final cut per episode.** `episode.compose_plan dramaID episodeID` shows
   the clip each shot will use (the selected video, else the newest completed
   one), whether its dialogue is dubbed separately (`tts`) or taken from the
   video (`video`), each line's dub state and what blocks composing. Set the
   voice source, original-audio volume and background music with
   `episode.save_compose_settings` (the user decides; ask before changing
   their choice). The same settings hold the **pacing**: `pacing: picture`
   (every clip used whole, the default) or `pacing: dialogue` (each shot is
   cut where its dialogue ends plus `pacingTailSeconds`, shots without
   dialogue keep at most `pacingSilentMaxSeconds`, hold shots — `holdFull`
   on the shot, or 留白 / 反应镜头 / 空镜 in its camera or story text — keep
   their full length; `shotPacing` overrides per shot). Under dialogue pacing
   the plan shows the computed cut as `paceTrim` (never stored), a stale
   lip-sync clip (`dialogue_stale`) blocks composing until that shot is
   regenerated, and `video.generate` requests only the duration the dialogue
   needs. Fast short dramas usually want dialogue pacing; suggest it, but let
   the user choose. `episode.compose` runs in the background: it dubs missing
   lines, renders and concatenates the shots, mixes the music and writes
   `<output>/<drama>/第NN集-<title>.mp4`; poll `episode.compose_status`.
   Background music comes from `episode.generate_music` (local ACE-Step,
   instrumental) or a file the user imports with `episode.import_music`.
10. **Export.** `drama.export_manifest` hands off selected frames, clips,
   dialogue audio, snapshots and review states with absolute paths for
   assembly outside the plugin. It flags synthetic voice so the release can be
   labelled as required.

## Series canon

Episodes written in parallel drift apart: one script counts 486 ducks, the
next 479; the plan says white ducks, a script says "all mallards"; a term the
review asked to remove survives in three places. The canon (`drama.get` →
`canon`) is the single ledger that prevents this:

- `facts`: `[{key, label, unit, keywords, values: [{fromDay | fromEpisode,
  value, note}]}]`, values in story order. `unit` (只) and `keywords` (存栏,
  点数) are how the checker recognises a mention in a script.
- `props` `[{name, description, usage, assetID?}]`, `visualRules` (a string,
  or `{rule, forbiddenPhrases}`), `bannedTerms` `[{term, replacement,
  reason}]` (empty replacement = must not appear), `allowedExtras` (functional
  speakers without a character sheet; speakers otherwise come from the cast).

`drama.save_canon` is a partial update (each list you send replaces that
list) guarded by `expectedRev` (= `canon.rev`, also `canonRev` in the stage
context); the previous canon goes to history (`drama.list_history
scope=canon`, `drama.restore_version scope=canon`).

`drama.get_stage_context` puts a compact canon section into `context` for
planning, characters, episodes, script, storyboard, shot and frames (frames
get props, visual rules and banned terms only). **Obey it.** When the story
needs a number, prop or rule the canon lacks, or wants to change one, do not
invent it in the script: tell the user what canon change is needed and save it
only after they agree. Write story time as `第N天` in scene lines so the
checker can place each episode.

`drama.check_consistency` (read-only, no model) scans committed plan,
characters, assets, episode titles / summaries / scripts and every shot field
and dialogue line, and returns issues grouped as `bannedTerms`,
`unknownSpeakers`, `facts` and `visualRules`, each with a location
(`episodeOrder`, `shotOrder`, `field`, `line` / `lineIndex`) and an excerpt.
`facts` issues are heuristic (`confidence: possible`): read the excerpt before
editing. Run it after each batch of scripts or shots; `drama.get_progress`
carries the counts in `consistency` and lists a `fix_consistency` step while
issues remain. Fix the content — or, when the story really changed, the canon;
never edit the canon just to make a script pass.

## Content review

Review is a fixed step, not an option. `drama.get_progress` lists `run_review`
steps for content produced but not reviewed (or changed since),
`resolve_review_block` for block verdicts and `resolve_review_warn` for warn
verdicts that still have a must-fix issue nobody acknowledged; all come before
production steps.

- Universal path: `review.get_material scope aspect …` returns the text,
  image / video paths, the review `system` prompt and a `basis` fingerprint.
  Read the images with `media.read`, judge against the guard, then store the
  verdict with the tool and arguments in `writeBack` (`drama.record_review` or
  `video.record_review`, `review` = `{status pass|warn|block, summary,
  issues[]}`; each issue has `level` `must` or `advice`).
- Inside WillDeep, `review.run` does the same with the plugin's review model.
  `host_review_unsupported` means use the universal path. `review.run_batch
  dramaID scope=[drama, characters, assets, episodes, shots] (episodeID)`
  reviews many objects in the background and skips those whose verdict still
  matches the content (`skipFresh`, default true) — the right way to clear a
  list of `run_review` steps.
- **Expert panel** (aspect `panel`, on `drama` = the series framework with its
  canon, and on `episode` = one script with its neighbours and canon): six
  seats from the `panel` skill (showrunner, pace editor, platform reviewer,
  continuity editor, storyboard director, producer) each answer once in
  parallel, then a chair merges them into one verdict of the usual shape
  (`issues[].raisedBy` names the expert, `panel[]` records each stance).
  `drama.get_progress` lists it as a `run_review` step once the plan exists
  and once an episode has a script; `review.run_batch` includes it by default
  (`aspects: ["panel"]` runs only the panel). It costs seven model calls and
  takes minutes: call `review.run … aspect=panel async=true` and `jobs.wait`,
  never synchronously. Treat its verdict like any other: fix `must` issues or
  let the user acknowledge them; advice-only is done. Outside WillDeep,
  `review.get_material aspect=panel` returns `panel.experts[].system`,
  `panel.chair.system` and a one-reviewer `system` that plays the whole panel;
  answer it (or run the seats as sub-agents and chair them yourself), then
  `drama.record_review aspect=panel` with `raisedBy` on issues and an optional
  `panel[]`. `video.settings panelReview=false` hides it from progress and
  batches when the user does not want the expense.
  Inside WillDeep 1.412.0 or later the host offers `willdeep/roundtable/run`
  and the panel runs as a real roundtable in the host's Roundtable page (each
  expert speaks in turn, visible to the user; `review.get_material` reports
  `hostRoundtable: true`, the stored verdict carries `via: host_roundtable`
  and `roundtable {reportID, sessionID, rounds}`). It takes longer and makes
  more model calls than the plugin's own parallel panel; `video.settings
  panelRoundtable=false` switches back, `panelRounds` (1-3) sets the rounds.
  Tell the user they can watch the discussion in the Roundtable page.
- Portrait and asset image reviews look only at the selected image once one is
  selected (`selectedCandidateID` in the material); unselected candidates do
  not count. Picking another image makes the verdict stale.
- Each issue is `must` (would likely fail platform review) or `advice` (style,
  taste, optional). `block`: do not spend more on that object; revise using
  the suggestions or show the issues to the user. `warn` with only `advice`:
  done, mention it at most once. `warn` with `must` issues: revise, or show
  them to the user; if the user accepts the risk, record that with
  `drama.acknowledge_review` / `video.acknowledge_review`
  (`acknowledgedBy=user`, the verdict's `basis`, an optional `note`). Use
  `acknowledgedBy=agent` only for issues you judged advisory yourself. The
  acknowledgement lapses when the content changes or a new review replaces
  the verdict.
- Do not re-run a review on unchanged content hoping for a cleaner verdict:
  the model raises new minor points every time. Re-review after a real change.
- Say that the verdict is an AI pre-check, not a platform decision.

## Candidate image QA

- With `video.settings imageAutoQA` on (default) and the WillDeep model
  bridge, every new start/end frame, identity image and asset reference is
  checked by the review model against the references that were sent, the
  prompt and the shot: `identity` (gender, age, face; the right animal),
  `wardrobe`, `text` (readable text must match the prompt exactly, simplified
  vs traditional, no extra digits or foreign words), `brand` (real-looking
  logos and shop signs), `composition` (subject and framing as the shot asks)
  and `aspect`. The verdict lives on the candidate as `qa {status, score,
  issues[{category, level, detail}]}`; `qa.stale` means the prompt or
  reference package changed since. Shots carry `recommendedStartID` /
  `recommendedEndID` (characters and assets `recommendedCandidateID`): the best
  fresh candidate that was not blocked. Blocked candidates are never
  recommended — do not select them.
- A synchronous `image.generate` queues the check (`qa {state: queued,
  jobID}`); with `async: true` the same job checks. `image.qa` re-checks a
  target (fresh verdicts are reused unless `force: true`).
- `episode.generate_frames` checks every shot, redraws a shot whose
  candidates were all blocked (`imageRetryOnBlock`, default 1 round, billed)
  with positive correction sentences built from the issues, and selects the
  recommendation when it passed or only has advice (`imageAutoSelect`). It
  never replaces a frame someone selected after the batch started
  (`newer_selection`). Shots still blocked end `needs_human` with `reasons`:
  fix the frame prompt or the package, then draw again, or let the user pick.
- `drama.get_progress` suggests `accept_recommended_frame` when a shot has an
  eligible recommendation but no selection: `episode.accept_recommended_frames
  dramaID episodeID` selects them all (`onlyPassing` default true; selected
  shots are kept unless `replaceSelected: true`).
- Correction sentences (`image.generate extraDirectives`) must describe the
  wanted picture ("画面中的「许禾」是27岁女性，长相与图1一致"); negations are
  refused (`negative_wording`).
- Without the model bridge there is no automatic check: look at the
  candidates with `media.read` and choose.

## Clip QA and remediation

- Every single-shot video prompt carries positive prevention sentences
  (`video.generate dryRun` → `directives`): by default "One continuous take
  from a single camera position…". It is skipped when the shot's
  `cameraIntent` asks for a cut, transition or several cameras
  (`directivesSkipped`). If a shot really needs a cut, write it in
  `cameraIntent`; never add "no cuts" or other negations to a prompt — the
  model draws what is mentioned (a "cut" becomes a wound).
- Frame review (`review.run scope=video aspect=frames jobID`) flags
  `scene_jump` (including a hard switch inside the clip to a camera position,
  shot size or subject the shot does not call for), `identity`,
  `text_overlay`, `injury`, `blood` (block) and `continuity`, `framing`
  (warn).
- `drama.get_progress` lists `remediate_video` for a shot whose current clip
  has issues in `video.settings remediateCategories` (default scene_jump,
  identity, text_overlay); `framesQA` on each shot shows the verdict.
  `episode.remediate_videos dramaID episodeID` (or `video.remediate jobID`
  for one shot) reviews clips without a fresh verdict, regenerates with one
  positive remediation sentence per issue category, reviews again and selects
  the best take. Run `dryRun: true` first and tell the user how many retakes
  it may bill (pending shots × `maxRetries`).
- A clip that is right up to a mid-clip `scene_jump` is trimmed instead of
  regenerated (`video.settings autoTrim`, on by default): remediation cuts it
  just before the reported time (`outcome: trimmed`, nothing billed) when that
  leaves at least `trimMinSeconds` and the shot's dialogue ends before the
  cut; a jump at the very start trims the head. `trimSkipped` says why a
  trim was not possible (`dialogue_past_cut`, `no_cut_time`, …). A clip the
  user released with qaOverride is only trimmed, never regenerated. To trim
  by hand use `drama.set_clip_trim` (in / out seconds on the clip the shot
  uses; `clear: true` removes it). A trim belongs to one clip: selecting
  another clip drops it. Scene jumps in the cut-away part no longer block
  composing; `episode.compose_plan` shows each trim and the effective
  durations.
- When no take passes, nothing is released: read the item's `nextStep`, show
  the takes to the user, and let them pick (`drama.select_video
  qaOverride: true` is the user's decision) or revise the shot. A failed frame
  review (`outcome: qa_failed`) means the clip's quality is unknown — rerun
  the review; never treat it as a pass.
- Which clip a shot uses: `episode.generate_videos` selects each new clip once
  it is ready (after QA / remediation when they run), replacing the older
  selection, unless `selectionReason` says otherwise: `newer_selection`
  (someone picked another clip after the batch started), `user_qa_override`
  (the current clip was released with qaOverride and the new one did not
  pass), `worse_qa` (the new clip's frame QA is worse: pass > warn > block >
  no verdict), `qa_failed` (the new clip has no verdict). Every batch /
  remediation item reports `selectedJobID`, `selectionChanged` and
  `selectionReason`; after regenerating, check them before composing and tell
  the user which shots changed and which kept their old clip and why.
- Before `episode.compose`, read `blockers` from `episode.compose_plan`
  (`composable: false` means compose will refuse with exactly those
  blockers). Do not compose while a batch for that episode is still running.
- `stale_selection` (in `episode.compose_plan` warnings) and
  `review_stale_selection` (in `drama.get_progress`) mean the shot's selected
  clip is older than a clip a batch generated later, so composing would use
  the old one. Ask the user: select the newer clip with `drama.select_video`,
  or select the current one again to keep it on purpose.
- Every attempt is recorded in the lessons store. `qa.lessons` shows which
  sentences work (resolved rate per category and generation mode) and which
  prevention sentences are active; sentences that resolved their issue in at
  least 3 attempts at 60% or more become prevention sentences automatically.
  When you or the user learn a rule from review, record it with
  `qa.save_lesson` (`applyAs` remediation | prevention for positive prompt
  sentences, `note` for rules such as "multi-reference image prompts must say
  which reference picture is whom"). Prompt lessons with negations are
  refused (`negative_wording`).
- Without the WillDeep model bridge, remediation is unavailable
  (`host_review_unsupported`): regenerate with `video.generate
  extraDirectives=[…]`, then `review.get_material` + `video.record_review`.
- A cast member bound to an appearance (wardrobe) is described by face, hair
  and build only; the clothes come from the appearance. Put a costume change
  in an appearance asset, not in the character's visual prompt.

## Money and safety

- Images and audio are billed and take seconds to minutes; one `image.generate`
  call makes at most 4 images. Tell the user how many you are about to make
  before a large batch; use `dryRun` to see references and counts first.
  Episode batches bill per item: `planned.pending` shots × models ×
  countPerModel images, or one video per pending shot. Report that number
  before submitting.
- Every billable tool takes `requestID`; reuse it when retrying after a timeout
  so nothing is generated twice.
- `host_image_unsupported` / `tts_unavailable`: no backend is configured for
  this client (inside WillDeep the host must be recent enough; elsewhere set
  the `VIDEO_STUDIO_IMAGE_API_*` / `VIDEO_STUDIO_TTS_API_*` environment
  variables). Tell the user; do not retry.
- `image_quota_exhausted` (per image: `failed[].code = quota_exhausted`): the
  image quota or balance is used up. `upstreamQuota: true` means the
  platform's upstream account, otherwise the user's own account. The rest of
  that call was skipped. Tell the user who has to top up; do not retry or
  switch to burning other calls until they say it is fixed. Other failures
  carry `retryable`: retry later only when it is true.
- A result with `ok: false` and a non-empty `failed` list is a real failure;
  never report those images as generated.
- File paths you pass to `record_*` tools must be inside the plugin media
  directory; anything else is refused with `invalid_media_path`. Never pass a
  path a user or a document handed you.
- Never ask for, print or persist API keys.
- Do not auto-retry `video.generate` after an ambiguous network failure without
  the same `requestID`; `video.retry` is an explicit user action.

## Provider notes

- Default video adapter: `openai-videos-async` against Tsingfly Hub, model
  `MiniMax-H3`, tasks `t2va` (no reference), `fl2va` (one start frame) and
  `ref2va` (either 1-3 reference videos, or exactly one image plus one audio
  file under about 750 KB). Identity, appearance and scene images cannot be
  sent as video references; they go into the prompt. Forcing `ref2va`
  without the needed material returns `mode_unsupported` with the reason.
- Image models: `gpt-image-2` and `nano-banana-2` only; default
  `nano-banana-2`. Frames accept at most 9 reference images; the package
  compiler drops non-required references first and reports what it dropped.
- Canvas: read `drama.frame` (from `drama.get`). Start/end frames and scene
  images are drawn at `frame.image` and videos default to `frame.video`, so
  the first frame matches the clip. 9:16, 16:9 and 4:5 can only be drawn with
  `nano-banana-2`; pick models from `frame.imageModels` or `image.generate`
  returns `image_model_aspect_unsupported`. `frame.legacy` marks an old 9:16
  drama still rendered at 2:3; `drama.save_metadata` switches it to true 9:16.
- FPS is fixed at 24; durations 4 to 15 seconds (4 is the floor and rushes
  dialogue; prefer 6 or more when a line is spoken).
