//! 执行层 v1：内核策略跑在 tokio 任务上。
//! - EDF/FIFO 准入、无望即拒；
//! - 交互式等待且槽位满时，abort 一个剩余最多的批处理任务，进度清零后重新排队（重算）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lacbs_core::model::Class;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const WORK_US: u64 = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Fifo,
    Edf,
}

#[derive(Clone, Copy)]
enum Sig {
    Admit,
    Resume,
    Step,
}

struct Record {
    ttft: Duration,
    met: bool,
}

struct Args {
    workers: usize,
    tasks: u32,
    secs: u64,
    max_seqs: usize,
    step_ms: u64,
    mode: Mode,
    interactive_pct: u32,
    preempt: bool,
    swap: bool,
    swap_slots: usize,
    swap_us: u64,
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
            interactive_pct: 30,
            preempt: true,
            swap: false,
            swap_slots: 4,
            swap_us: 2_000,
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
                "--interactive-pct" => a.interactive_pct = val()?.parse()?,
                "--no-preempt" => a.preempt = false,
                "--swap" => a.swap = true,
                "--swap-slots" => a.swap_slots = val()?.parse()?,
                "--swap-us" => a.swap_us = val()?.parse()?,
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
        if a.max_seqs == 0 {
            bail!("--max-seqs 不能为 0");
        }
        Ok(a)
    }
}

struct JobState {
    produced: AtomicU32,
    first_ms: AtomicU64,
    slo_us: u64,
}

struct Pending {
    id: u32,
    class: Class,
    arrival: Instant,
    deadline: Instant,
    prompt: u32,
    output: u32,
    state: Arc<JobState>,
    ready_at: Instant,
    /// 换出后恢复：跳过 prefill，从已产出的 token 继续
    resume: bool,
    /// 是否占着一个换出槽位（准入或拒绝时归还）
    swapped: bool,
}

struct Running {
    id: u32,
    class: Class,
    arrival: Instant,
    deadline: Instant,
    prompt: u32,
    output: u32,
    state: Arc<JobState>,
    tx: mpsc::Sender<Sig>,
    handle: JoinHandle<()>,
}

struct Arrival {
    id: u32,
    class: Class,
    at: Instant,
    deadline: Instant,
    prompt: u32,
    output: u32,
    slo_us: u64,
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

    let summary = obs.rt.block_on(run(&args))?;
    println!(
        "{{\"tasks\":{},\"completed\":{},\"rejected\":{},\"met_ttft\":{},\"ttft_p50_ms\":{:.1},\"ttft_p99_ms\":{:.1},\"ticks\":{},\"max_running\":{},\"preemptions\":{},\"recompute_tokens\":{},\"swaps\":{},\"stalls\":{},\"elapsed_ms\":{}}}",
        summary.tasks,
        summary.completed,
        summary.rejected,
        summary.met,
        summary.ttft_p50,
        summary.ttft_p99,
        summary.ticks,
        summary.max_running,
        summary.preemptions,
        summary.recompute_tokens,
        summary.swaps,
        watch.stall_count(),
        summary.elapsed_ms
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
    preemptions: u64,
    recompute_tokens: u64,
    swaps: u64,
    elapsed_ms: u128,
}

async fn run(args: &Args) -> anyhow::Result<Summary> {
    let started = Instant::now();
    let tasks = args.tasks;
    let pct = args.interactive_pct.min(100);
    let gap = Duration::from_millis((args.secs * 1000 / u64::from(tasks.max(1))).max(1));
    let tick = Duration::from_millis(args.step_ms.max(1));

    let (arr_tx, mut arr_rx) = mpsc::unbounded_channel::<Arrival>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Record>();

    let _producer = tokio::spawn(async move {
        for id in 0..tasks {
            let interactive = id % 100 < pct;
            let (class, prompt, output, slo_us) = if interactive {
                (Class::Interactive, 64, 6 + (id % 4) * 2, 60_000)
            } else {
                (Class::Batch, 128, 24 + (id % 6) * 4, 400_000)
            };
            let at = Instant::now();
            if arr_tx
                .send(Arrival {
                    id,
                    class,
                    at,
                    deadline: at + Duration::from_micros(slo_us),
                    prompt,
                    output,
                    slo_us,
                })
                .is_err()
            {
                break;
            }
            tokio::time::sleep(gap).await;
        }
    });

    let mut waiting: Vec<Pending> = Vec::new();
    let mut running: Vec<Running> = Vec::new();
    let mut arrivals_open = true;
    let (mut completed, mut rejected, mut met) = (0u32, 0u32, 0u32);
    let mut ttfts: Vec<f64> = Vec::new();
    let mut ticks = 0u64;
    let mut max_running = 0usize;
    let (mut preemptions, mut recompute_tokens) = (0u64, 0u64);
    let mut swap_used = 0usize;
    let mut swaps = 0u64;

    loop {
        if arrivals_open {
            while let Ok(a) = arr_rx.try_recv() {
                waiting.push(Pending {
                    id: a.id,
                    class: a.class,
                    arrival: a.at,
                    deadline: a.deadline,
                    prompt: a.prompt,
                    output: a.output,
                    state: Arc::new(JobState {
                        produced: AtomicU32::new(0),
                        first_ms: AtomicU64::new(0),
                        slo_us: a.slo_us,
                    }),
                    ready_at: Instant::now(),
                    resume: false,
                    swapped: false,
                });
            }
            if arr_rx.is_closed() {
                arrivals_open = false;
            }
        }

        // 拒绝：还没出过首字、且已过截止期
        let now = Instant::now();
        let mut i = 0;
        while i < waiting.len() {
            let w = &waiting[i];
            if w.state.first_ms.load(Ordering::Relaxed) == 0 && now >= w.deadline {
                if w.swapped {
                    swap_used = swap_used.saturating_sub(1);
                }
                waiting.remove(i);
                rejected += 1;
            } else {
                i += 1;
            }
        }

        match args.mode {
            Mode::Edf => waiting.sort_by_key(|w| (w.deadline, w.id)),
            Mode::Fifo => waiting.sort_by_key(|w| w.id),
        }
        let mut i = 0;
        while i < waiting.len() {
            if running.len() < args.max_seqs {
                if waiting[i].ready_at > Instant::now() {
                    i += 1;
                    continue;
                }
                let job = waiting.remove(i);
                if job.swapped {
                    swap_used = swap_used.saturating_sub(1);
                }
                spawn_job(job, &done_tx, &mut running);
                continue;
            }
            if !args.preempt || waiting[i].class != Class::Interactive {
                break;
            }
            let victim = running
                .iter()
                .enumerate()
                .filter(|(_, r)| r.class == Class::Batch)
                .max_by_key(|(_, r)| {
                    r.output
                        .saturating_sub(r.state.produced.load(Ordering::Relaxed))
                })
                .map(|(idx, _)| idx);
            let Some(v) = victim else { break };
            let r = running.remove(v);
            r.handle.abort();
            let produced = r.state.produced.load(Ordering::Relaxed);
            preemptions += 1;
            let can_swap = args.swap && swap_used < args.swap_slots;
            if can_swap {
                swap_used += 1;
                swaps += 1;
            } else {
                recompute_tokens += u64::from(r.prompt) + u64::from(produced);
                r.state.produced.store(0, Ordering::Relaxed);
            }
            let delay = if can_swap {
                Duration::from_micros(args.swap_us)
            } else {
                tick
            };
            waiting.push(Pending {
                id: r.id,
                class: r.class,
                arrival: r.arrival,
                deadline: r.deadline,
                prompt: r.prompt,
                output: r.output,
                state: r.state,
                ready_at: Instant::now() + delay,
                resume: can_swap,
                swapped: can_swap,
            });
        }

        for r in &running {
            let _ = r.tx.try_send(Sig::Step);
        }
        ticks += 1;
        max_running = max_running.max(running.len());

        while let Ok(rec) = done_rx.try_recv() {
            completed += 1;
            if rec.met {
                met += 1;
            }
            ttfts.push(rec.ttft.as_secs_f64() * 1000.0);
        }
        running.retain(|r| !r.tx.is_closed());

        if !arrivals_open && waiting.is_empty() && running.is_empty() {
            tokio::task::yield_now().await;
            while let Ok(rec) = done_rx.try_recv() {
                completed += 1;
                if rec.met {
                    met += 1;
                }
                ttfts.push(rec.ttft.as_secs_f64() * 1000.0);
            }
            if done_rx.is_empty() {
                break;
            }
        }
        tokio::time::sleep(tick).await;
    }

    ttfts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct_fn = |p: f64| -> f64 {
        if ttfts.is_empty() {
            0.0
        } else {
            ttfts[((p / 100.0) * (ttfts.len() - 1) as f64).round() as usize]
        }
    };

    Ok(Summary {
        tasks,
        completed,
        rejected,
        met,
        ttft_p50: pct_fn(50.0),
        ttft_p99: pct_fn(99.0),
        ticks,
        max_running,
        preemptions,
        recompute_tokens,
        swaps,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn spawn_job(job: Pending, done: &mpsc::UnboundedSender<Record>, running: &mut Vec<Running>) {
    let (tx, rx) = mpsc::channel::<Sig>(4);
    let first = if job.resume { Sig::Resume } else { Sig::Admit };
    let _ = tx.try_send(first);
    let state = job.state.clone();
    let arrival = job.arrival;
    let output = job.output;
    let prompt = job.prompt;
    let done = done.clone();
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        match rx.recv().await {
            Some(Sig::Admit) => spin(Duration::from_micros(u64::from(prompt))),
            Some(Sig::Resume) => {}
            _ => return,
        }
        while state.produced.load(Ordering::Relaxed) < output {
            match rx.recv().await {
                Some(Sig::Step) => {
                    spin(Duration::from_micros(WORK_US));
                    let n = state.produced.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 {
                        state
                            .first_ms
                            .store(arrival.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                }
                _ => return,
            }
        }
        let ttft = Duration::from_millis(state.first_ms.load(Ordering::Relaxed));
        let met = ttft.as_micros() as u64 <= state.slo_us;
        let _ = done.send(Record { ttft, met });
    });
    running.push(Running {
        id: job.id,
        class: job.class,
        arrival: job.arrival,
        deadline: job.deadline,
        prompt: job.prompt,
        output: job.output,
        state: job.state,
        tx,
        handle,
    });
}
