//! 执行层 v0：把内核的策略（EDF 准入 / 无望即拒）真的跑在 tokio 任务上。
//! 每个 tick 代表一次引擎迭代：给每个在跑请求发一个"出词许可"，任务拿到后做一次计算。
//! 抢占暂未接入（任务取消语义留给下一版），其余与 kernel 的语义一致。

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lacbs_core::Request;
use tokio::sync::mpsc;

const WORK_US: u64 = 200; // 每次 decode 的计算量（模拟）

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Fifo,
    Edf,
}

#[derive(Clone, Copy)]
enum Sig {
    Admit,
    Step,
    Reject,
}

enum Outcome {
    Completed { ttft: Duration },
    Rejected,
}

struct Record {
    outcome: Outcome,
    met: bool,
}

struct Args {
    workers: usize,
    tasks: u32,
    secs: u64,
    max_seqs: usize,
    step_ms: u64,
    mode: Mode,
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut a = Args {
            workers: 4,
            tasks: 64,
            secs: 5,
            max_seqs: 16,
            step_ms: 8,
            mode: Mode::Edf,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut val = || it.next().context(format!("{flag} 缺值"));
            match flag.as_str() {
                "--workers" => a.workers = val()?.parse()?,
                "--tasks" => a.tasks = val()?.parse()?,
                "--secs" => a.secs = val()?.parse()?,
                "--max-seqs" => a.max_seqs = val()?.parse()?,
                "--step-ms" => a.step_ms = val()?.parse()?,
                "--mode" => {
                    a.mode = match val()?.as_str() {
                        "fifo" => Mode::Fifo,
                        "edf" => Mode::Edf,
                        other => bail!("未知 mode: {other}"),
                    }
                }
                other => bail!("未知参数: {other}"),
            }
        }
        Ok(a)
    }
}

fn spin(d: Duration) {
    let start = Instant::now();
    let mut x = 0u64;
    while start.elapsed() < d {
        for _ in 0..512 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
    }
    std::hint::black_box(x);
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    lacbs_obs::init_tracing();
    let obs = lacbs_obs::build_runtime(args.workers)?;
    let watch = lacbs_obs::start_stall_watch(&obs.rt, Duration::from_millis(400));

    let summary = obs.rt.block_on(run(&args));
    let stalls = watch.stall_count();
    let s = summary?;
    println!(
        "{{\"tasks\":{},\"completed\":{},\"rejected\":{},\"met_ttft\":{},\"ttft_p50_ms\":{:.1},\"ttft_p99_ms\":{:.1},\"ticks\":{},\"max_running\":{},\"stalls\":{},\"elapsed_ms\":{}}}",
        s.tasks,
        s.completed,
        s.rejected,
        s.met,
        s.ttft_p50,
        s.ttft_p99,
        s.ticks,
        s.max_running,
        stalls,
        s.elapsed_ms
    );
    Ok(())
}

struct Summary {
    tasks: u32,
    completed: u32,
    rejected: u32,
    met: u32,
    ttft_p50: f64,
    ttft_p99: f64,
    ticks: u64,
    max_running: usize,
    elapsed_ms: u128,
}

struct JobCtx {
    id: u32,
    deadline: Instant,
    tx: mpsc::Sender<Sig>,
}

async fn run(args: &Args) -> anyhow::Result<Summary> {
    let started = Instant::now();
    let (arr_tx, mut arr_rx) = mpsc::unbounded_channel::<Request>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Record>();

    // 到达发生器：在 secs 内均匀投放
    let tasks = args.tasks;
    let gap = Duration::from_millis((args.secs * 1000 / tasks.max(1) as u64).max(1));
    let _producer = tokio::spawn(async move {
        for id in 0..tasks {
            let req = Request {
                id,
                attempt: 0,
                class: lacbs_core::model::Class::Interactive,
                arrival_us: 0,
                prompt_tokens: 64 + (id % 4) * 16,
                output_tokens: 8 + (id % 5) * 4,
                ttft_slo_us: 120_000,
                tpot_slo_us: 100_000,
            };
            if arr_tx.send(req).is_err() {
                break;
            }
            tokio::time::sleep(gap).await;
        }
    });

    let tick = Duration::from_millis(args.step_ms.max(1));
    let mut waiting: Vec<JobCtx> = Vec::new();
    let mut running: Vec<JobCtx> = Vec::new();
    let mut ticks = 0u64;
    let mut max_running = 0usize;
    let (mut completed, mut rejected, mut met) = (0u32, 0u32, 0u32);
    let mut ttfts: Vec<f64> = Vec::new();
    let mut arrivals_open = true;

    loop {
        // 1) 收新请求，起任务
        if arrivals_open {
            while let Ok(req) = arr_rx.try_recv() {
                let arrival = Instant::now();
                let deadline = arrival + Duration::from_micros(req.ttft_slo_us);
                let (tx, rx) = mpsc::channel::<Sig>(4);
                let done = done_tx.clone();
                let output = req.output_tokens;
                let prompt = req.prompt_tokens;
                let ttft_slo = req.ttft_slo_us;
                tokio::spawn(async move {
                    job_loop(prompt, output, ttft_slo, arrival, rx, done).await;
                });
                waiting.push(JobCtx {
                    id: req.id,
                    deadline,
                    tx,
                });
            }
            if arr_rx.is_closed() {
                arrivals_open = false;
            }
        }

        // 2) 拒绝无望
        let now = Instant::now();
        waiting.retain(|j| {
            if now >= j.deadline {
                let _ = j.tx.try_send(Sig::Reject);
                false
            } else {
                true
            }
        });

        // 3) 准入
        match args.mode {
            Mode::Edf => waiting.sort_by_key(|j| (j.deadline, j.id)),
            Mode::Fifo => waiting.sort_by_key(|j| j.id),
        }
        while running.len() < args.max_seqs {
            let Some(job) = waiting.first() else { break };
            if job.tx.try_send(Sig::Admit).is_err() {
                waiting.remove(0);
                continue;
            }
            let job = waiting.remove(0);
            running.push(job);
        }

        // 4) 每个在跑的任务发一个出词许可
        for j in &running {
            let _ = j.tx.try_send(Sig::Step);
        }
        ticks += 1;
        max_running = max_running.max(running.len());

        // 5) 回收完成的槽位
        while let Ok(rec) = done_rx.try_recv() {
            match rec.outcome {
                Outcome::Completed { ttft } => {
                    completed += 1;
                    if rec.met {
                        met += 1;
                    }
                    ttfts.push(ttft.as_secs_f64() * 1000.0);
                }
                Outcome::Rejected => rejected += 1,
            }
        }
        // 任务结束后会自行退出；用 running 中 tx 的 closed 状态回收槽位
        running.retain(|j| !j.tx.is_closed());

        if !arrivals_open && waiting.is_empty() && running.is_empty() {
            // 等 done 队列排空（任务可能刚退出）
            tokio::task::yield_now().await;
            while let Ok(rec) = done_rx.try_recv() {
                match rec.outcome {
                    Outcome::Completed { ttft } => {
                        completed += 1;
                        if rec.met {
                            met += 1;
                        }
                        ttfts.push(ttft.as_secs_f64() * 1000.0);
                    }
                    Outcome::Rejected => rejected += 1,
                }
            }
            if done_rx.is_empty() {
                break;
            }
        }

        tokio::time::sleep(tick).await;
    }

    ttfts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if ttfts.is_empty() {
            0.0
        } else {
            ttfts[((p / 100.0) * (ttfts.len() - 1) as f64).round() as usize]
        }
    };

    Ok(Summary {
        tasks: args.tasks,
        completed,
        rejected,
        met,
        ttft_p50: pct(50.0),
        ttft_p99: pct(99.0),
        ticks,
        max_running,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

async fn job_loop(
    prompt_tokens: u32,
    output_tokens: u32,
    ttft_slo_us: u64,
    arrival: Instant,
    mut rx: mpsc::Receiver<Sig>,
    done: mpsc::UnboundedSender<Record>,
) {
    match rx.recv().await {
        Some(Sig::Admit) => {
            // 首字前的 prefill 计算
            spin(Duration::from_micros(u64::from(prompt_tokens)));
        }
        _ => {
            let _ = done.send(Record {
                outcome: Outcome::Rejected,
                met: false,
            });
            return;
        }
    }
    let mut produced = 0u32;
    let mut ttft: Option<Duration> = None;
    while produced < output_tokens {
        match rx.recv().await {
            Some(Sig::Step) => {
                spin(Duration::from_micros(WORK_US));
                if produced == 0 {
                    ttft = Some(arrival.elapsed());
                }
                produced += 1;
            }
            // None = 调度器丢弃了通道（本版不会发生）；Admit/Reject 属异常序列
            _ => break,
        }
    }
    let ttft = ttft.unwrap_or_else(|| arrival.elapsed());
    let met = ttft.as_micros() as u64 <= ttft_slo_us;
    let _ = done.send(Record {
        outcome: Outcome::Completed { ttft },
        met,
    });
}
