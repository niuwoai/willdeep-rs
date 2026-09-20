//! 从游标尾随 Runtime 事件流。
//!
//! ```bash
//! cargo run -p willdeep-runtime-client --example tail_events        # 只看新事件
//! cargo run -p willdeep-runtime-client --example tail_events -- 0   # 从头回放
//! ```
//!
//! 流被服务端关掉（空闲关闭、`willdeep daemon upgrade` 交接）就重读 `daemon.json`
//! 重连，沿最后一个序号续传，不重复也不漏。

mod common;

use willdeep_runtime_protocol::RuntimeEvent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = common::connect()?;
    let mut after = match std::env::args().nth(1) {
        Some(text) => text
            .parse::<u64>()
            .map_err(|_| "the cursor must be an event sequence number")?,
        None => common::unwrap(client.status().await?)?.event_sequence,
    };
    loop {
        let mut stream = client.stream_events(after, 500, None).await?;
        while let Some(envelope) = stream.next::<RuntimeEvent>().await? {
            let event = common::unwrap(envelope)?;
            after = event.sequence;
            println!(
                "#{} {} {} {}",
                event.sequence, event.timestamp, event.kind, event.message
            );
        }
        eprintln!("event stream closed after #{after}; reconnecting");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        client = common::connect()?;
    }
}
