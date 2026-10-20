//! 无头探针：从 tokio-console 的 gRPC 面取一轮更新，断言"真的有任务被观测到"。
//! 有数据 = 退出码 0；连不上或零任务 = 退出码 1（可直接进 CI）。

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, bail};
use console_api::instrument::InstrumentRequest;
use console_api::instrument::instrument_client::InstrumentClient;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut addr = String::from("http://127.0.0.1:6669");
    let mut secs = 5u64;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--addr" => addr = it.next().context("--addr 缺值")?,
            "--secs" => secs = it.next().context("--secs 缺值")?.parse()?,
            other => bail!("未知参数: {other}"),
        }
    }

    let mut client = InstrumentClient::connect(addr.clone())
        .await
        .with_context(|| format!("连不上 {addr}（harness 带 console feature 起了吗）"))?;
    let mut stream = client
        .watch_updates(InstrumentRequest {})
        .await
        .context("instrument RPC 失败")?
        .into_inner();

    let (mut spawned, mut polls, mut wakes, mut updates) = (0usize, 0u64, 0u64, 0u64);
    let mut ids: HashSet<u64> = HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);

    while let Some(remain) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        match tokio::time::timeout(remain, stream.message()).await {
            Ok(Ok(Some(update))) => {
                updates += 1;
                if let Some(t) = update.task_update {
                    spawned += t.new_tasks.len();
                    for (id, stats) in t.stats_update {
                        ids.insert(id);
                        polls += stats.poll_stats.map_or(0, |p| p.polls);
                        wakes += stats.wakes;
                    }
                }
            }
            Ok(Ok(None)) | Err(_) => break,
            Ok(Err(e)) => return Err(e).context("读流失败"),
        }
    }

    println!(
        "{{\"updates\":{updates},\"spawned\":{spawned},\"distinct_tasks\":{},\"polls\":{polls},\"wakes\":{wakes}}}",
        ids.len()
    );
    if ids.is_empty() || polls == 0 {
        bail!("观测面没数据：distinct_tasks={} polls={polls}", ids.len());
    }
    Ok(())
}
