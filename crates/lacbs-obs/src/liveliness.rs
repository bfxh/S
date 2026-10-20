//! 执行器停滞看门狗：监控任务定期打点，独立线程发现打点变旧就报 STALL。
//! 抓的是"所有 worker 都被同步代码占住、唤醒事件排不上队"的软死锁。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_liveliness_monitor::LivelinessMonitor;

pub struct StallWatch {
    pub stalls: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    monitor: Arc<LivelinessMonitor>,
}

impl StallWatch {
    pub fn stall_count(&self) -> u64 {
        self.stalls.load(Ordering::Relaxed)
    }

    pub fn last_age(&self) -> Duration {
        Instant::now().saturating_duration_since(self.monitor.latest_report())
    }
}

impl Drop for StallWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(rt: &tokio::runtime::Runtime, threshold: Duration) -> StallWatch {
    let (_task, monitor) = {
        let _guard = rt.enter();
        LivelinessMonitor::start(|fut| tokio::spawn(fut))
    };
    let stalls = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let s = stalls.clone();
    let st = stop.clone();
    let m = monitor.clone();
    let thread = std::thread::Builder::new()
        .name("stall-watch".into())
        .spawn(move || {
            let mut in_stall = false;
            while !st.load(Ordering::Relaxed) {
                std::thread::sleep(threshold / 4);
                let silent = Instant::now().saturating_duration_since(m.latest_report());
                if std::env::var_os("LACBS_STALL_DEBUG").is_some() {
                    eprintln!("stall-watch: age={}ms", silent.as_millis());
                }
                if silent > threshold {
                    if !in_stall {
                        s.fetch_add(1, Ordering::Relaxed);
                        in_stall = true;
                        eprintln!(
                            "STALL: 执行器 {} ms 没打过点（阈值 {} ms）",
                            silent.as_millis(),
                            threshold.as_millis()
                        );
                    }
                } else {
                    in_stall = false;
                }
            }
        })
        .expect("起看门狗线程");

    StallWatch {
        stalls,
        stop,
        thread: Some(thread),
        monitor,
    }
}
