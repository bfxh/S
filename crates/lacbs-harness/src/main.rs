use std::hint::black_box;
use std::time::{Duration, Instant};

use anyhow::bail;

const DECODE_SLEEP: Duration = Duration::from_millis(2);
const DECODE_BURN: Duration = Duration::from_micros(50);
const PREFILL_BURN: Duration = Duration::from_millis(3);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Decode,
    Prefill,
    Mixed,
    Stall,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "decode" => Some(Self::Decode),
            "prefill" => Some(Self::Prefill),
            "mixed" => Some(Self::Mixed),
            "stall" => Some(Self::Stall),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Prefill => "prefill",
            Self::Mixed => "mixed",
            Self::Stall => "stall",
        }
    }
}

struct Args {
    workers: usize,
    tasks: usize,
    secs: u64,
    mode: Mode,
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut args = Args {
            workers: 4,
            tasks: 64,
            secs: 5,
            mode: Mode::Mixed,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| anyhow::anyhow!("{flag} 缺值"));
            match flag.as_str() {
                "--workers" => args.workers = value()?.parse()?,
                "--tasks" => args.tasks = value()?.parse()?,
                "--secs" => args.secs = value()?.parse()?,
                "--mode" => {
                    let v = value()?;
                    args.mode = Mode::parse(&v).ok_or_else(|| anyhow::anyhow!("未知 mode: {v}"))?;
                }
                other => bail!("未知参数: {other}"),
            }
        }
        Ok(args)
    }
}

fn main() -> anyhow::Result<()> {
    let _hp = lacbs_obs::init_hotpath("harness");
    let args = Args::parse()?;
    lacbs_obs::init_tracing();

    let obs = lacbs_obs::build_runtime(args.workers)?;
    let watch = lacbs_obs::start_stall_watch(&obs.rt, Duration::from_millis(400));
    let started = Instant::now();
    let iterations = obs.rt.block_on(run(&args));
    let elapsed = started.elapsed();

    println!(
        "{{\"workers\":{},\"tasks\":{},\"mode\":\"{}\",\"iterations\":{},\"elapsed_ms\":{},\"iters_per_task\":{:.1},\"stalls\":{}}}",
        args.workers,
        args.tasks,
        args.mode.as_str(),
        iterations,
        elapsed.as_millis(),
        iterations as f64 / args.tasks as f64,
        watch.stall_count()
    );
    Ok(())
}

async fn run(args: &Args) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(args.secs);
    if args.mode == Mode::Stall {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entered = Arc::new(AtomicUsize::new(0));
        for _ in 0..args.workers {
            let entered = entered.clone();
            tokio::spawn(async move {
                entered.fetch_add(1, Ordering::SeqCst);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1200));
                }
            });
        }
        // 等阻塞任务真正占住 worker，否则后面的 decode 任务会先被抢到
        tokio::time::sleep(Duration::from_millis(200)).await;
        eprintln!(
            "进入阻塞的 worker 数 = {}/{}",
            entered.load(Ordering::SeqCst),
            args.workers
        );
    }
    let mut handles = Vec::with_capacity(args.tasks);
    for id in 0..args.tasks {
        let mode = match args.mode {
            Mode::Mixed if id % 4 == 0 => Mode::Prefill,
            Mode::Mixed => Mode::Decode,
            Mode::Stall => Mode::Decode,
            m => m,
        };
        handles.push(tokio::spawn(worker(id, mode, deadline)));
    }
    let mut total = 0;
    for h in handles {
        total += h.await.unwrap_or(0);
    }
    total
}

async fn worker(id: usize, mode: Mode, deadline: Instant) -> u64 {
    let mut n = 0u64;
    while Instant::now() < deadline {
        match mode {
            Mode::Decode => {
                tokio::time::sleep(DECODE_SLEEP).await;
                burn(DECODE_BURN);
            }
            Mode::Prefill => {
                burn(PREFILL_BURN);
                tokio::task::yield_now().await;
            }
            Mode::Mixed | Mode::Stall => unreachable!("混合/停滞模式已在派发时展开"),
        }
        n += 1;
        black_box(id);
    }
    n
}

/// 确定性 CPU 占用，用于制造"异步皮、同步骨"的 poll 长尾。
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn burn(d: Duration) {
    let start = Instant::now();
    let mut x = 0u64;
    while start.elapsed() < d {
        for _ in 0..2048 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
    }
    black_box(x);
}
