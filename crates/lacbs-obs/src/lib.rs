//! 观测面装配：一个工具一个 feature，互不牵连。
//!
//! | feature | 工具 | Windows | Linux |
//! |---|---|---|---|
//! | `console` | tokio-console | ✅ 实测 | 未测 |
//! | `dial9` | dial9 飞行记录仪 | 待验证 | 未测 |
//! | `hotpath` | hotpath | 待验证 | 未测 |
//! | `liveliness` | async-liveliness-monitor | 待验证 | 未测 |

use std::path::{Path, PathBuf};

pub const CONSOLE_PORT: u16 = 6669;

#[cfg(feature = "console")]
pub fn init_tracing() {
    console_subscriber::init();
}

#[cfg(not(feature = "console"))]
pub fn init_tracing() {}

/// 运行时要由 dial9 建（它必须在 runtime 之前挂钩子），所以统一从这里拿。
pub struct ObsRuntime {
    pub rt: tokio::runtime::Runtime,
    #[cfg(feature = "dial9")]
    _recorder: Option<dial9::Recorder>,
}

pub fn build_runtime(workers: usize) -> anyhow::Result<ObsRuntime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(workers).enable_all();

    #[cfg(feature = "dial9")]
    {
        use dial9::Dial9HandleTokioExt;
        let dir = trace_dir();
        std::fs::create_dir_all(&dir)?;
        eprintln!("dial9: 录制到 {}", dir.display());
        let writer = dial9::DiskBuffer::builder()
            .base_path(&dir)
            .max_total_size(64 * 1024 * 1024)
            .max_file_size(8 * 1024 * 1024)
            .build();
        let recorder = dial9::recorder_or_disabled(writer).build();
        let rt = recorder.handle().attach_tokio_runtime(
            builder,
            dial9::TokioAttachOptions::builder()
                .runtime_name("main")
                .task_tracking_enabled(true)
                .build(),
        )?;
        return Ok(ObsRuntime {
            rt,
            _recorder: Some(recorder),
        });
    }

    #[cfg(not(feature = "dial9"))]
    {
        let _ = trace_dir;
        Ok(ObsRuntime {
            rt: builder.build()?,
        })
    }
}

fn trace_dir() -> PathBuf {
    std::env::var_os("LACBS_TRACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("traces").to_path_buf())
}

#[cfg(feature = "dial9")]
pub fn trace_path() -> PathBuf {
    trace_dir()
}

#[cfg(feature = "liveliness")]
mod liveliness;
#[cfg(feature = "liveliness")]
pub use liveliness::{StallWatch, start as start_stall_watch};

/// 没开 `liveliness` 时的空实现：调用点不用分叉。
#[cfg(not(feature = "liveliness"))]
pub struct StallWatch;

#[cfg(not(feature = "liveliness"))]
impl StallWatch {
    pub fn stall_count(&self) -> u64 {
        0
    }
}

#[cfg(not(feature = "liveliness"))]
pub fn start_stall_watch(
    _rt: &tokio::runtime::Runtime,
    _threshold: std::time::Duration,
) -> StallWatch {
    StallWatch
}

#[cfg(feature = "hotpath")]
pub use hotpath::HotpathGuard;

#[cfg(feature = "hotpath")]
pub fn init_hotpath(name: &'static str) -> HotpathGuard {
    hotpath::HotpathGuardBuilder::new(name).build()
}

#[cfg(not(feature = "hotpath"))]
pub struct HotpathGuard;

#[cfg(not(feature = "hotpath"))]
pub fn init_hotpath(_name: &'static str) -> HotpathGuard {
    HotpathGuard
}
