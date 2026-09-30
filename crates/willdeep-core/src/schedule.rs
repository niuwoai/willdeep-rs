//! 定时任务（`willdeep.scheduler` 内置插件的数据面）。
//!
//! 行为对齐 Xedit 的 Automations（`AgentRoutine`）：每次触发以任务的 prompt 为
//! 第一句开一个**全新会话**；带 `goal` 的任务每次运行都被要求判断目标是否达成，
//! 达成就调 `complete_scheduled_task` 把自己删掉。调度 JSON 与 Xedit 同构
//! （`kind` + `minutes` / `hour` / `minute` / `weekday`，weekday 1 = 周日 … 7 = 周六）。
//!
//! 存在 `$WILLDEEP_HOME/schedules.json`：插件的 MCP 进程写、daemon 的调度器读写，
//! 两个进程共用，所以每次读改写都拿 `schedules.lock` 的排他文件锁，整文件原子
//! 替换，权限 0600。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const SECONDS_PER_DAY: i64 = 86_400;

/// 何时触发。本地时间按调用方给的 UTC 偏移（秒）换算。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    /// 每 N 分钟，从上次运行（没跑过则从创建时）算起。
    Interval { minutes: u32 },
    /// 每天本地 HH:MM。
    DailyAt { hour: u8, minute: u8 },
    /// 周一到周五本地 HH:MM。
    WeekdaysAt { hour: u8, minute: u8 },
    /// 每周某天本地 HH:MM。`weekday` 1 = 周日 … 7 = 周六（与 Foundation 一致）。
    WeeklyAt { weekday: u8, hour: u8, minute: u8 },
}

impl Schedule {
    /// 校验取值范围；建任务时调用，坏数据不落盘。
    pub fn validate(&self) -> Result<(), String> {
        let time_ok = |hour: u8, minute: u8| hour <= 23 && minute <= 59;
        match *self {
            Self::Interval { minutes: 0 } => Err("interval_minutes must be at least 1".to_owned()),
            Self::Interval { .. } => Ok(()),
            Self::DailyAt { hour, minute } | Self::WeekdaysAt { hour, minute }
                if !time_ok(hour, minute) =>
            {
                Err("time must be HH:MM in 24-hour local time".to_owned())
            }
            Self::WeeklyAt {
                weekday,
                hour,
                minute,
            } if !(1..=7).contains(&weekday) || !time_ok(hour, minute) => {
                Err("weekly schedules need weekday 1-7 (1 = Sunday) and HH:MM".to_owned())
            }
            _ => Ok(()),
        }
    }

    /// 一行人读的描述。
    pub fn summary(&self) -> String {
        match *self {
            Self::Interval { minutes } if minutes % 60 == 0 && minutes >= 60 => {
                match minutes / 60 {
                    1 => "every hour".to_owned(),
                    hours => format!("every {hours} hours"),
                }
            }
            Self::Interval { minutes } => format!("every {minutes} minutes"),
            Self::DailyAt { hour, minute } => format!("daily at {hour:02}:{minute:02}"),
            Self::WeekdaysAt { hour, minute } => {
                format!("weekdays at {hour:02}:{minute:02}")
            }
            Self::WeeklyAt {
                weekday,
                hour,
                minute,
            } => format!("every {} at {hour:02}:{minute:02}", weekday_name(weekday)),
        }
    }

    /// `after`（Unix 秒）之后下一次该触发的时刻（Unix 秒）。`utc_offset` 是
    /// 本地时间相对 UTC 的秒数。
    pub fn next_fire(&self, after: u64, utc_offset: i64) -> Option<u64> {
        match *self {
            Self::Interval { minutes } => (minutes > 0).then(|| after + u64::from(minutes) * 60),
            Self::DailyAt { hour, minute } => next_local(after, utc_offset, hour, minute, |_| true),
            Self::WeekdaysAt { hour, minute } => {
                next_local(after, utc_offset, hour, minute, |weekday| {
                    (2..=6).contains(&weekday)
                })
            }
            Self::WeeklyAt {
                weekday,
                hour,
                minute,
            } => next_local(after, utc_offset, hour, minute, |day| day == weekday),
        }
    }
}

fn weekday_name(weekday: u8) -> &'static str {
    match weekday {
        1 => "Sunday",
        2 => "Monday",
        3 => "Tuesday",
        4 => "Wednesday",
        5 => "Thursday",
        6 => "Friday",
        7 => "Saturday",
        _ => "?",
    }
}

/// 严格晚于 `after` 的第一个「本地 HH:MM 且星期满足条件」的时刻。
fn next_local(
    after: u64,
    utc_offset: i64,
    hour: u8,
    minute: u8,
    weekday_ok: impl Fn(u8) -> bool,
) -> Option<u64> {
    let local_after = after as i64 + utc_offset;
    let day_start = local_after.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
    let at = i64::from(hour) * 3_600 + i64::from(minute) * 60;
    for day in 0..=8 {
        let candidate = day_start + day * SECONDS_PER_DAY + at;
        if candidate <= local_after {
            continue;
        }
        // 1970-01-01 是周四；Foundation 的 weekday 1 = 周日。
        let weekday = ((candidate.div_euclid(SECONDS_PER_DAY) + 4).rem_euclid(7) + 1) as u8;
        if weekday_ok(weekday) {
            return u64::try_from(candidate - utc_offset).ok();
        }
    }
    None
}

/// 本机在 `at`（Unix 秒）那一刻相对 UTC 的偏移秒数；非 Unix 平台按 UTC。
#[cfg(unix)]
// `tm_gmtoff` 是 `c_long`：64 位平台上已是 i64，32 位上不是。
#[allow(clippy::unnecessary_cast)]
pub fn local_utc_offset(at: u64) -> i64 {
    let time = at as libc::time_t;
    // SAFETY: localtime_r only writes the caller-owned `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&time, &mut tm) };
    if converted.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
pub fn local_utc_offset(_at: u64) -> i64 {
    0
}

/// 一个定时任务。新增字段一律 `serde(default)`：旧文件照常能读。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: Uuid,
    pub name: String,
    /// 每次触发时新会话的第一句话。必须自给自足：新会话看不到别的上下文。
    pub prompt: String,
    /// 新会话绑定的工作区。
    pub workspace: PathBuf,
    pub schedule: Schedule,
    /// 自我完成的目标。有它时每次运行都被要求判断是否达成。
    #[serde(default)]
    pub goal: Option<String>,
    /// 这类无人值守运行的审批档位（`strict` / `smart` / `workspace-write` /
    /// `full-access`）。不写按工作区档位。
    #[serde(default)]
    pub approval_mode: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub created_at: u64,
    #[serde(default)]
    pub last_run_at: Option<u64>,
    #[serde(default)]
    pub last_session_id: Option<Uuid>,
    /// 在哪个会话里用 `schedule_task` 建的（从侧栏 / CLI 建的为空）。
    #[serde(default)]
    pub origin_session_id: Option<Uuid>,
}

fn default_enabled() -> bool {
    true
}

impl ScheduledTask {
    /// 下一次该触发的时刻；停用或调度退化时为 `None`。
    pub fn next_fire(&self, utc_offset: i64) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        self.schedule
            .next_fire(self.last_run_at.unwrap_or(self.created_at), utc_offset)
    }

    /// 到 `now` 为止是否到期。停机期间错过多少次都只算一次：调度器触发后把
    /// `last_run_at` 设成 `now`，下一次从 `now` 往后算。
    pub fn is_due(&self, now: u64, utc_offset: i64) -> bool {
        self.next_fire(utc_offset).is_some_and(|due| due <= now)
    }

    /// 触发时新会话收到的第一句话：任务 prompt，带 goal 时附上自我完成说明
    /// （与 Xedit `AppStateAgentRoutines` 同一段话术）。
    pub fn run_prompt(&self) -> String {
        let mut prompt = format!(
            "[scheduled-task id={} name={:?} schedule={:?}] This is an unattended scheduled run in a fresh session; no one is watching live.\n\n{}",
            self.id,
            self.name,
            self.schedule.summary(),
            self.prompt
        );
        if let Some(goal) = self.goal.as_deref().filter(|goal| !goal.trim().is_empty()) {
            prompt.push_str(&format!(
                "\n\n[scheduled-task-goal]\nGoal: {goal}\nAfter doing the work above, judge whether this goal is now met:\n- Met → call the `complete_scheduled_task` tool with task_id \"{}\" so this task is removed and never runs again.\n- Not met → briefly report current progress. Do not call complete_scheduled_task.",
                self.id
            ));
        }
        prompt
    }
}

/// `$WILLDEEP_HOME/schedules.json` 的读写。
#[derive(Clone, Debug)]
pub struct ScheduleStore {
    path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    tasks: Vec<ScheduledTask>,
}

impl ScheduleStore {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join("schedules.json"),
            lock_path: home.join("schedules.lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Result<Vec<ScheduledTask>, String> {
        self.update(|tasks| tasks.clone())
    }

    /// 持锁读改写：`edit` 改完的列表整文件原子替换回去。
    pub fn update<R>(&self, edit: impl FnOnce(&mut Vec<ScheduledTask>) -> R) -> Result<R, String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)
            .map_err(|error| format!("open {}: {error}", self.lock_path.display()))?;
        lock.lock()
            .map_err(|error| format!("lock {}: {error}", self.lock_path.display()))?;
        let mut file = match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str::<StoreFile>(&text)
                .map_err(|error| format!("parse {}: {error}", self.path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoreFile::default(),
            Err(error) => return Err(format!("read {}: {error}", self.path.display())),
        };
        let before = file.tasks.clone();
        let result = edit(&mut file.tasks);
        if file.tasks != before {
            file.version = 1;
            self.write(&file)?;
        }
        Ok(result)
    }

    fn write(&self, file: &StoreFile) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(file).map_err(|error| error.to_string())?;
        let temporary = self
            .path
            .with_extension(format!("json.{}.tmp", Uuid::new_v4().simple()));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options
            .open(&temporary)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        handle
            .write_all(&text)
            .and_then(|()| handle.sync_all())
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("replace {}: {error}", self.path.display()))
    }
}

/// 解析 `HH:MM`（24 小时制）。
pub fn parse_hhmm(raw: &str) -> Option<(u8, u8)> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let hour: u8 = hour.trim().parse().ok()?;
    let minute: u8 = minute.trim().parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-30T00:00:00Z，周三。
    const WEDNESDAY_MIDNIGHT_UTC: u64 = 1_790_726_400;
    const BEIJING: i64 = 8 * 3_600;

    #[test]
    fn schedules_round_trip_in_the_xedit_shape() {
        let json = serde_json::to_value(Schedule::WeeklyAt {
            weekday: 2,
            hour: 9,
            minute: 30,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "weeklyAt", "weekday": 2, "hour": 9, "minute": 30})
        );
        let interval: Schedule =
            serde_json::from_value(serde_json::json!({"kind": "interval", "minutes": 30})).unwrap();
        assert_eq!(interval, Schedule::Interval { minutes: 30 });
    }

    #[test]
    fn next_fire_follows_local_time() {
        // 北京时间 08:00 周三。
        let now = WEDNESDAY_MIDNIGHT_UTC;
        let daily = Schedule::DailyAt { hour: 9, minute: 0 };
        // 当天 09:00 北京 = 01:00Z。
        assert_eq!(daily.next_fire(now, BEIJING), Some(now + 3_600));
        // 过了点就到明天。
        assert_eq!(
            daily.next_fire(now + 3_600, BEIJING),
            Some(now + 3_600 + 86_400)
        );
        assert_eq!(
            Schedule::Interval { minutes: 30 }.next_fire(now, BEIJING),
            Some(now + 1_800)
        );
        // 周五 18:00 之后的工作日 09:00 是下周一。
        let friday_evening_local = now + 2 * 86_400 + 10 * 3_600; // 周五 18:00 北京
        let weekdays = Schedule::WeekdaysAt { hour: 9, minute: 0 };
        assert_eq!(
            weekdays.next_fire(friday_evening_local, BEIJING),
            Some(now + 5 * 86_400 + 3_600)
        );
        // 每周日 10:00 北京 = 周日 02:00Z。
        let sunday = Schedule::WeeklyAt {
            weekday: 1,
            hour: 10,
            minute: 0,
        };
        assert_eq!(
            sunday.next_fire(now, BEIJING),
            Some(now + 4 * 86_400 + 2 * 3_600)
        );
    }

    #[test]
    fn validation_and_hhmm_parsing() {
        assert!(Schedule::Interval { minutes: 0 }.validate().is_err());
        assert!(
            Schedule::DailyAt {
                hour: 24,
                minute: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            Schedule::WeeklyAt {
                weekday: 8,
                hour: 1,
                minute: 0
            }
            .validate()
            .is_err()
        );
        assert_eq!(parse_hhmm(" 07:05 "), Some((7, 5)));
        assert_eq!(parse_hhmm("7:60"), None);
        assert_eq!(parse_hhmm("noon"), None);
        assert_eq!(
            Schedule::Interval { minutes: 120 }.summary(),
            "every 2 hours"
        );
        assert_eq!(
            Schedule::DailyAt { hour: 9, minute: 5 }.summary(),
            "daily at 09:05"
        );
    }

    fn task(schedule: Schedule) -> ScheduledTask {
        ScheduledTask {
            id: Uuid::new_v4(),
            name: "check CI".to_owned(),
            prompt: "Check CI on main".to_owned(),
            workspace: PathBuf::from("/ws"),
            schedule,
            goal: None,
            approval_mode: None,
            enabled: true,
            created_at: WEDNESDAY_MIDNIGHT_UTC,
            last_run_at: None,
            last_session_id: None,
            origin_session_id: None,
        }
    }

    #[test]
    fn missed_runs_collapse_into_one() {
        let mut every_ten = task(Schedule::Interval { minutes: 10 });
        let much_later = WEDNESDAY_MIDNIGHT_UTC + 3 * 3_600;
        assert!(every_ten.is_due(much_later, 0));
        every_ten.last_run_at = Some(much_later);
        assert!(
            !every_ten.is_due(much_later + 60, 0),
            "fires once, not 18 times"
        );
        every_ten.enabled = false;
        assert!(!every_ten.is_due(much_later + 3_600, 0));
    }

    #[test]
    fn goal_tasks_are_told_how_to_complete_themselves() {
        let mut watch = task(Schedule::Interval { minutes: 30 });
        assert!(!watch.run_prompt().contains("complete_scheduled_task"));
        watch.goal = Some("CI on main is green".to_owned());
        let prompt = watch.run_prompt();
        assert!(prompt.contains("Check CI on main"));
        assert!(prompt.contains("CI on main is green"));
        assert!(prompt.contains(&format!("task_id \"{}\"", watch.id)));
    }

    #[test]
    fn the_store_round_trips_and_is_private() {
        let home = std::env::temp_dir().join(format!("willdeep-schedules-{}", Uuid::new_v4()));
        let store = ScheduleStore::new(&home);
        assert!(store.list().unwrap().is_empty());
        let created = task(Schedule::DailyAt { hour: 9, minute: 0 });
        store.update(|tasks| tasks.push(created.clone())).unwrap();
        assert_eq!(store.list().unwrap(), vec![created.clone()]);
        let removed = store
            .update(|tasks| {
                let before = tasks.len();
                tasks.retain(|task| task.id != created.id);
                before != tasks.len()
            })
            .unwrap();
        assert!(removed);
        assert!(store.list().unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
