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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Victim {
    /// 剩余输出最多（原策略）
    Longest,
    /// 剩余输出最少（最先能跑完）
    Shortest,
    /// 剩余 slack 最多（最不着急）
    Slack,
    /// 混合：α×归一化 slack + (1−α)×(1−完成比例)，越大越先换出
    Hybrid,
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
    victim: Victim,
    hybrid_alpha: f64,
    batch_slo_us: u64,
    arrivals_csv: Option<String>,
    slice_s: f64,
    slice_start_s: f64,
    target_load: f64,
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
            victim: Victim::Longest,
            hybrid_alpha: 0.5,
            batch_slo_us: 400_000,
            arrivals_csv: None,
            slice_s: 60.0,
            slice_start_s: 0.0,
            target_load: 0.8,
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
                "--hybrid-alpha" => a.hybrid_alpha = val()?.parse()?,
                "--batch-slo-us" => a.batch_slo_us = val()?.parse()?,
                "--arrivals-csv" => a.arrivals_csv = Some(val()?),
                "--slice-s" => a.slice_s = val()?.parse()?,
                "--slice-start-s" => a.slice_start_s = val()?.parse()?,
                "--target-load" => a.target_load = val()?.parse()?,
                "--victim" => {
                    a.victim = match val()?.as_str() {
                        "longest" => Victim::Longest,
                        "shortest" => Victim::Shortest,
                        "slack" => Victim::Slack,
                        "hybrid" => Victim::Hybrid,
                        other => bail!("未知 victim: {other}"),
                    }
                }
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
        "{{\"tasks\":{},\"completed\":{},\"rejected\":{},\"met_ttft\":{},\"ttft_p50_ms\":{:.1},\"ttft_p99_ms\":{:.1},\"ticks\":{},\"max_running\":{},\"preemptions\":{},\"recompute_tokens\":{},\"swaps\":{},\"victim\":\"{}\",\"source\":\"{}\",\"stalls\":{},\"elapsed_ms\":{}}}",
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
        summary.victim,
        summary.source,
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
    victim: &'static str,
    source: &'static str,
    elapsed_ms: u128,
}

async fn run(args: &Args) -> anyhow::Result<Summary> {
    let started = Instant::now();
    let tasks = args.tasks;
    let pct = args.interactive_pct.min(100);
    let gap = Duration::from_millis((args.secs * 1000 / u64::from(tasks.max(1))).max(1));
    let tick = Duration::from_millis(args.step_ms.max(1));
    let batch_slo = args.batch_slo_us;

    let (arr_tx, mut arr_rx) = mpsc::unbounded_channel::<Arrival>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Record>();

    // 到达计划：(墙钟偏移秒, 类别, prompt, output, slo_us, id)
    let (plan, source, wall_s) = match args.arrivals_csv.as_deref() {
        Some(path) => {
            let p = build_trace_plan(path, args, batch_slo)?;
            let wall = p.last().map_or(0.0, |x| x.0);
            eprintln!(
                "trace arrivals: {} 条，切片 {:.0}s，墙钟约 {:.1}s（target_load={:.2}）",
                p.len(),
                args.slice_s,
                wall,
                args.target_load
            );
            (p, "csv", wall)
        }
        None => {
            let mut p = Vec::with_capacity(tasks as usize);
            for id in 0..tasks {
                let interactive = id % 100 < pct;
                let (class, prompt, output, slo_us) = if interactive {
                    (Class::Interactive, 64, 6 + (id % 4) * 2, 60_000)
                } else {
                    (Class::Batch, 128, 24 + (id % 6) * 4, batch_slo)
                };
                p.push((
                    id as f64 * gap.as_secs_f64(),
                    class,
                    prompt,
                    output,
                    slo_us,
                    id,
                ));
            }
            (p, "uniform", 0.0)
        }
    };
    let _ = wall_s;
    let n_tasks = plan.len() as u32;

    let _producer = tokio::spawn(async move {
        let base = Instant::now();
        for (offset_s, class, prompt, output, slo_us, id) in plan {
            let target = base + Duration::from_secs_f64(offset_s);
            let now = Instant::now();
            if target > now {
                tokio::time::sleep(target - now).await;
            }
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
            let now = Instant::now();
            let tick_ms = i128::from(args.step_ms.max(1));
            let score = |r: &Running| -> i128 {
                let remaining = i128::from(
                    r.output
                        .saturating_sub(r.state.produced.load(Ordering::Relaxed)),
                );
                match args.victim {
                    Victim::Longest => remaining,
                    Victim::Shortest => -remaining,
                    Victim::Slack => {
                        let slack = r.deadline.saturating_duration_since(now).as_millis() as i128;
                        slack - remaining * tick_ms
                    }
                    Victim::Hybrid => {
                        let produced = i128::from(r.state.produced.load(Ordering::Relaxed));
                        let slack_ms = r.deadline.saturating_duration_since(now).as_millis() as f64
                            - remaining as f64 * tick_ms as f64;
                        let slack_norm = (slack_ms / 400.0).clamp(-2.0, 2.0);
                        let progress = if r.output == 0 {
                            1.0
                        } else {
                            produced as f64 / f64::from(r.output)
                        };
                        let score = args.hybrid_alpha * slack_norm
                            + (1.0 - args.hybrid_alpha) * (1.0 - progress);
                        (score * 1_000_000.0) as i128
                    }
                }
            };
            let victim = running
                .iter()
                .enumerate()
                .filter(|(_, r)| r.class == Class::Batch)
                .max_by_key(|(_, r)| (score(r), r.id))
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
        tasks: n_tasks,
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
        victim: match args.victim {
            Victim::Longest => "longest",
            Victim::Shortest => "shortest",
            Victim::Slack => "slack",
            Victim::Hybrid => "hybrid",
        },
        source,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// (墙钟偏移秒, 类别, prompt, output, slo_us, id)
type PlanItem = (f64, Class, u32, u32, u64, u32);

/// 读回放 CSV，按容量把 trace 时间缩放到墙钟，只取前 `slice_s` 秒。
fn build_trace_plan(path: &str, args: &Args, batch_slo: u64) -> anyhow::Result<Vec<PlanItem>> {
    let text = std::fs::read_to_string(path)?;
    let mut rows: Vec<(f64, Class, u32, u32)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let mut it = line.split(',');
        let t: f64 = it.next().unwrap_or("").trim().parse()?;
        let p: u32 = it.next().unwrap_or("").trim().parse()?;
        let o: u32 = it.next().unwrap_or("").trim().parse()?;
        let class = match it.next().unwrap_or("batch").trim() {
            "interactive" => Class::Interactive,
            _ => Class::Batch,
        };
        if p > 0 && o > 0 && t >= args.slice_start_s && t < args.slice_start_s + args.slice_s {
            rows.push((t, class, p, o));
        }
    }
    if rows.is_empty() {
        anyhow::bail!("{path} 在 {:.0}s 切片内没有请求", args.slice_s);
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("时间非 NaN"));

    let n = rows.len() as f64;
    let avg_out = rows.iter().map(|r| f64::from(r.3)).sum::<f64>() / n;
    let step_s = args.step_ms.max(1) as f64 / 1000.0;
    let capacity = args.max_seqs as f64 / (avg_out * step_s);
    let observed = n / args.slice_s;
    let k = observed / (args.target_load.max(0.01) * capacity);
    eprintln!(
        "trace: rows={n} observed={observed:.2}/s capacity={capacity:.2}/s scale={k:.4} avg_out={avg_out:.0}"
    );

    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(id, (t, class, p, o))| {
            let slo = if class == Class::Interactive {
                60_000
            } else {
                batch_slo
            };
            ((t - args.slice_start_s) * k, class, p, o, slo, id as u32)
        })
        .collect())
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
