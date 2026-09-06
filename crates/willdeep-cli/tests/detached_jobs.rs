#[cfg(unix)]
mod tests {
    use std::path::PathBuf;
    use willdeep_core::detached_job::MAX_JOB_OUTPUT_BYTES;
    use willdeep_core::{DetachedJob, DetachedJobStore, JobState};

    fn store() -> (DetachedJobStore, PathBuf) {
        let home = std::env::temp_dir().join(format!("willdeep-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        (
            DetachedJobStore::new(&home)
                .with_supervisor_executable(PathBuf::from(env!("CARGO_BIN_EXE_willdeep"))),
            home,
        )
    }

    fn wait_for_finish(store: &DetachedJobStore, job: &DetachedJob) -> JobState {
        for _ in 0..100 {
            let state = store.state(job);
            if state != JobState::Running {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("job never finished");
    }

    #[test]
    fn detached_deadline_stops_delayed_side_effect() {
        let (store, home) = store();
        let policy = willdeep_core::sandbox::SandboxSpec::new(
            willdeep_core::sandbox::SandboxPolicy::Off,
            [],
        );
        let job = store
            .spawn_with_policy(
                "sleep 2; touch should-not-exist",
                "timeout",
                &home,
                &policy,
                1,
            )
            .unwrap();
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 124 }
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!home.join("should-not-exist").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn logs_are_readable_while_running_and_survive_timeout() {
        let (store, home) = store();
        let policy = willdeep_core::sandbox::SandboxSpec::new(
            willdeep_core::sandbox::SandboxPolicy::Off,
            [],
        );
        let job = store
            .spawn_with_policy(
                "printf live-stdout; printf live-stderr >&2; sleep 30",
                "live logs",
                &home,
                &policy,
                2,
            )
            .unwrap();
        let mut observed = false;
        // Supervisor startup is outside the command's two-second deadline.
        // Allow launch latency while still requiring a Running observation.
        let observation_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < observation_deadline {
            let report = store.report(&job);
            if report.output.contains("live-stdout") && report.output.contains("live-stderr") {
                assert_eq!(report.state, JobState::Running);
                observed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(observed, "logs only appeared after completion");
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 124 }
        );
        let output = store.report(&job).output;
        assert!(output.contains("live-stdout") && output.contains("live-stderr"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn detached_read_only_policy_cannot_write() {
        let (store, home) = store();
        let policy = willdeep_core::sandbox::SandboxSpec::new(
            willdeep_core::sandbox::SandboxPolicy::ReadOnly,
            [],
        );
        match store.spawn_with_policy("touch should-not-exist", "readonly", &home, &policy, 10) {
            Ok(job) => assert_ne!(
                wait_for_finish(&store, &job),
                JobState::Finished { exit_code: 0 }
            ),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::Unsupported),
        }
        assert!(!home.join("should-not-exist").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn terminating_supervisor_cleans_command_descendants() {
        let (store, home) = store();
        let job = store
            .spawn(
                "touch ready; sleep 2; touch should-not-exist",
                "terminate",
                &home,
            )
            .unwrap();
        for _ in 0..100 {
            if home.join("ready").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(home.join("ready").exists());
        assert!(
            std::process::Command::new("kill")
                .arg(job.pid.to_string())
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 137 }
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!home.join("should-not-exist").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn detached_output_is_bounded_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let (store, home) = store();
        let job = store
            .spawn(
                "i=0; while [ $i -lt 30000 ]; do printf abcdefgh; i=$((i+1)); done",
                "output",
                &home,
            )
            .unwrap();
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 0 }
        );
        assert!(
            std::fs::metadata(store.directory().join(&job.id).join("stdout.log"))
                .unwrap()
                .len()
                <= 128 * 1024
        );
        let directory = store.directory().join(&job.id);
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(directory.join("stdout.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[ignore = "subprocess entry used by survives_actual_parent_exit"]
    fn spawn_and_exit_helper() {
        let home =
            PathBuf::from(std::env::var_os("WILLDEEP_DETACHED_TEST_HOME").expect("test home"));
        let store = DetachedJobStore::new(&home)
            .with_supervisor_executable(PathBuf::from(env!("CARGO_BIN_EXE_willdeep")));
        store
            .spawn("sleep 1; printf once >> effects", "restart", &home)
            .unwrap();
    }

    #[test]
    fn survives_actual_parent_exit_without_replay() {
        let (store, home) = store();
        let parent = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::spawn_and_exit_helper", "--ignored"])
            .env("WILLDEEP_DETACHED_TEST_HOME", &home)
            .status()
            .unwrap();
        assert!(parent.success());
        let jobs = store.list();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            wait_for_finish(&store, &jobs[0]),
            JobState::Finished { exit_code: 0 }
        );
        let reopened = DetachedJobStore::new(&home);
        assert_eq!(
            reopened.report(&jobs[0]).state,
            JobState::Finished { exit_code: 0 }
        );
        assert_eq!(
            std::fs::read_to_string(home.join("effects")).unwrap(),
            "once"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    /// 退出码从文件里读回来，进程早已消失也照样有结论。
    #[test]
    fn a_finished_job_reports_its_exit_code_after_the_process_is_gone() {
        let (store, home) = store();
        let job = store
            .spawn("printf hello; exit 3", "greet", &home)
            .expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 3 }
        );
        assert!(
            store
                .output(&job.id, MAX_JOB_OUTPUT_BYTES)
                .contains("hello")
        );
    }

    /// 成功与失败靠退出码分，不靠「进程还在不在」。
    #[test]
    fn success_and_failure_are_told_apart_by_the_recorded_code() {
        let (store, home) = store();
        let ok = store.spawn("true", "ok", &home).expect("spawn");
        let bad = store.spawn("exit 7", "bad", &home).expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &ok),
            JobState::Finished { exit_code: 0 }
        );
        assert_eq!(
            wait_for_finish(&store, &bad),
            JobState::Finished { exit_code: 7 }
        );
    }

    /// 记录跨进程可读：换一个 store 实例（等价于重启）照样取得回结果。
    #[test]
    fn a_restart_reads_the_result_instead_of_rerunning() {
        let (store, home) = store();
        let job = store.spawn("printf done", "job", &home).expect("spawn");
        wait_for_finish(&store, &job);

        let reopened = DetachedJobStore::new(&home)
            .with_supervisor_executable(PathBuf::from(env!("CARGO_BIN_EXE_willdeep")));
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        let report = reopened.report(&listed[0]);
        assert_eq!(report.state, JobState::Finished { exit_code: 0 });
        assert!(report.output.contains("done"));
        assert_eq!(report.job.command, "printf done");
    }

    /// 没留下退出码的进程是「不知道」，不是「失败」。
    #[test]
    fn a_vanished_process_is_unknown_not_failed() {
        let (store, home) = store();
        let mut job = store.spawn("true", "gone", &home).expect("spawn");
        wait_for_finish(&store, &job);
        // 手工抹掉收尸文件，模拟被 kill -9 或断电。
        std::fs::remove_file(store.directory().join(&job.id).join("exit")).expect("remove");
        // 顺便把 PID 改成一个几乎不可能存在的值。
        job.pid = 4_294_967_294;
        job.started_marker = None;
        assert_eq!(store.state(&job), JobState::Vanished);
    }

    /// PID 被复用时不能把别人的进程当成自己的作业。
    #[test]
    fn a_recycled_pid_does_not_look_like_a_running_job() {
        let (store, home) = store();
        let mut job = store.spawn("sleep 30", "sleeper", &home).expect("spawn");
        assert_eq!(store.state(&job), JobState::Running);
        // 同一个 PID，但启动时刻对不上：那是另一个进程。
        job.started_marker = Some("Thu Jan  1 00:00:00 1970".to_owned());
        assert_eq!(store.state(&job), JobState::Vanished);
        let _ = std::process::Command::new("kill")
            .arg(job.pid.to_string())
            .status();
    }

    /// 还在跑的作业不给删：删了那个进程就没人认领了。
    #[test]
    fn a_running_job_cannot_be_forgotten() {
        let (store, home) = store();
        let job = store.spawn("sleep 30", "sleeper", &home).expect("spawn");
        let error = store.forget(&job.id).expect_err("still running");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        let _ = std::process::Command::new("kill")
            .arg(job.pid.to_string())
            .status();
    }

    /// 路径里有空格和引号时，收尸文件仍然写在该写的地方。
    #[test]
    fn quoting_survives_awkward_paths() {
        let home = std::env::temp_dir().join(format!("willdeep jobs '{}'", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        let store = DetachedJobStore::new(&home)
            .with_supervisor_executable(PathBuf::from(env!("CARGO_BIN_EXE_willdeep")));
        let job = store.spawn("exit 5", "quoted", &home).expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 5 }
        );
    }
}
