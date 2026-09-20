# CI 样例

把 `willdeep run` 放进流水线的三份样例，说明见 [`docs/CI_INTEGRATION.md`](../../docs/CI_INTEGRATION.md)。

| 文件 | 用途 |
|---|---|
| `run-task.sh` | 通用脚本：装好 `willdeep` 后，读提示词文件跑一轮，`result.json` 与审计报告落到 `willdeep-artifacts/`，退出码原样交给作业 |
| `github-actions.yml` | GitHub Actions：`workflow_dispatch` 输入提示词，装发行包、跑脚本、上传产物 |
| `gitlab-ci.yml` | GitLab CI 同一件事 |

三份文件的语法在本仓库 CI 里校验（`bash -n`、YAML 解析）；真跑模型要凭据、要花钱，不进公共 CI。
