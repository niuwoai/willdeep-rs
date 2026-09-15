use super::{Session, SessionError};
use sha2::{Digest, Sha256};

struct DigestWriter(Sha256);

impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn fingerprint(session: &Session) -> Result<[u8; 32], SessionError> {
    let mut writer = DigestWriter(Sha256::new());
    // These fields form one execution snapshot. Never merge messages separately
    // from their checkpoint or compression boundary.
    //
    // 这里**只放对话本身**。`attention_read`、`runtime_event_cursor`、
    // `runtime_managed` 三项曾经也在这份快照里，但它们不是对话内容：前两项
    // 是每个客户端自己的视图状态（读过哪些提醒、事件流读到哪），第三项是路由
    // 归属的开关。把它们算进执行指纹，等于让「TUI 记一下自己读到第几条事件」
    // 与「守护进程写入这一轮的输出」互相判成执行冲突——一条会话同时被这两者
    // 持有正是运行时托管的**常态**，于是每次模型输出到达，两边必有一个被打死：
    // 先是 TUI 在「结果刚要显示」时退出，把 TUI 那侧改稳之后，换成守护进程的
    // 回合直接失败（failure_domain=internal）。两种都让用户白等一轮。
    //
    // 移出去之后它们按「后写者生效」合并：书签写偏了下次事件就自愈，而真正
    // 需要保护的东西——消息、计划、压缩边界、执行检查点——一项没少。
    serde_json::to_writer(
        &mut writer,
        &(
            session.version,
            session.id,
            &session.workspace,
            session.created_at,
            &session.messages,
            &session.current_plan,
            session.compression_generation,
            &session.compression_checkpoint,
            &session.manual_compression_usage,
            &session.execution_checkpoint,
        ),
    )?;
    Ok(writer.0.finalize().into())
}

pub(super) fn copy(source: &Session, target: &mut Session) {
    target.version = source.version;
    target.id = source.id;
    target.workspace = source.workspace.clone();
    target.created_at = source.created_at;
    target.messages = source.messages.clone();
    target.current_plan = source.current_plan.clone();
    // 与 `fingerprint` 同一条分界：视图状态与路由归属不属于执行快照，
    // 所以这里也不覆盖调用方手上的那一份。
    target.compression_generation = source.compression_generation;
    target.compression_checkpoint = source.compression_checkpoint.clone();
    target.manual_compression_usage = source.manual_compression_usage.clone();
    target.execution_checkpoint = source.execution_checkpoint.clone();
}
