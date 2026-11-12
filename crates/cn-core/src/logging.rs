//! 文件日志。
//!
//! 设计要点（都是踩过的坑）：
//! - **服务模式下绝不写 stdout/stderr**：Session 0 没有控制台，`println!` 轻则丢日志、
//!   重则拖住进程。所以 `echo` 只由 `--console` 打开。
//! - 按天切文件：`logs\svc-20270101.log`，写入前比对日期，跨天自动换文件。
//! - 每条日志带自增 `seq`，以后 GUI 通过管道增量拉取时用它做游标。
//! - 初始化失败不能 panic：日志写不出来时退化为"只回显不落盘"，服务必须还能跑。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::util;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Err = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    pub fn tag(self) -> &'static str {
        match self {
            Level::Err => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
        }
    }

    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_uppercase().as_str() {
            "ERR" | "ERROR" => Level::Err,
            "WARN" | "WARNING" => Level::Warn,
            "DEBUG" | "DBG" => Level::Debug,
            _ => Level::Info,
        }
    }
}

struct State {
    dir: PathBuf,
    echo: AtomicBool,
    level: AtomicU8,
    /// `--debug` 强制级别：为 true 时忽略配置里的 logLevel
    forced: AtomicBool,
    seq: AtomicU64,
    /// (日期, 文件)
    file: Mutex<Option<(String, File)>>,
    path: Mutex<Option<PathBuf>>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// 初始化日志。`echo=true` 时同时打印到控制台（只给 `--console` 用）。
pub fn init(dir: &Path, echo: bool, level: Level) -> std::io::Result<PathBuf> {
    if STATE.get().is_some() {
        // 重复初始化：只更新开关
        if let Some(s) = STATE.get() {
            s.echo.store(echo, Ordering::Relaxed);
            s.level.store(level as u8, Ordering::Relaxed);
        }
        return Ok(current_path().unwrap_or_default());
    }

    std::fs::create_dir_all(dir)?;
    let st = State {
        dir: dir.to_path_buf(),
        echo: AtomicBool::new(echo),
        level: AtomicU8::new(level as u8),
        forced: AtomicBool::new(false),
        seq: AtomicU64::new(0),
        file: Mutex::new(None),
        path: Mutex::new(None),
    };
    // 先把"今天应该写的文件"记下来，这样启动横幅里就能显示路径（文件在首次写入时才创建）
    let initial = dir.join(format!("svc-{}.log", util::today_key()));
    if let Ok(mut g) = st.path.lock() {
        *g = Some(initial.clone());
    }
    let _ = STATE.set(st);
    Ok(initial)
}

fn state() -> Option<&'static State> {
    STATE.get()
}

/// 当前日志文件路径（未初始化或还没写过时为 None）
pub fn current_path() -> Option<PathBuf> {
    let s = state()?;
    s.path.lock().ok().and_then(|g| g.clone())
}

/// 配置里的 logLevel 生效（除非 `--debug` 已经强制过）
pub fn set_level(level: Level) {
    if let Some(s) = state() {
        if s.forced.load(Ordering::Relaxed) {
            return;
        }
        s.level.store(level as u8, Ordering::Relaxed);
    }
}

/// `--debug`：强制 DEBUG 级别，后续 `set_level`（配置热加载）不再覆盖
pub fn force_debug() {
    if let Some(s) = state() {
        s.level.store(Level::Debug as u8, Ordering::Relaxed);
        s.forced.store(true, Ordering::Relaxed);
    }
}

pub fn level() -> Level {
    match state().map(|s| s.level.load(Ordering::Relaxed)).unwrap_or(2) {
        0 => Level::Err,
        1 => Level::Warn,
        3 => Level::Debug,
        _ => Level::Info,
    }
}

pub fn set_echo(echo: bool) {
    if let Some(s) = state() {
        s.echo.store(echo, Ordering::Relaxed);
    }
}

pub fn seq() -> u64 {
    state().map(|s| s.seq.load(Ordering::Relaxed)).unwrap_or(0)
}

fn open_for(date: &str) -> std::io::Result<(File, PathBuf)> {
    let s = state().ok_or_else(|| std::io::Error::other("logging not initialized"))?;
    std::fs::create_dir_all(&s.dir)?;
    let path = s.dir.join(format!("svc-{date}.log"));
    let f = OpenOptions::new().create(true).append(true).open(&path)?;
    Ok((f, path))
}

/// 写一条日志
pub fn write(level: Level, msg: &str) {
    let Some(s) = state() else {
        // 未初始化：只回显
        eprintln!("{} [{}] {}", util::now_text(), level.tag(), msg);
        return;
    };
    if (level as u8) > s.level.load(Ordering::Relaxed) {
        return;
    }

    let n = s.seq.fetch_add(1, Ordering::Relaxed) + 1;
    let line = format!(
        "{} [{}] [{:>6}] {}",
        util::now_text(),
        level.tag(),
        n,
        msg
    );

    if s.echo.load(Ordering::Relaxed) {
        if level == Level::Err {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    let today = util::today_key();
    let mut guard = match s.file.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let need_open = match guard.as_ref() {
        Some((d, _)) => d != &today,
        None => true,
    };
    if need_open {
        match open_for(&today) {
            Ok((f, p)) => {
                *guard = Some((today.clone(), f));
                if let Ok(mut g) = s.path.lock() {
                    *g = Some(p);
                }
            }
            Err(e) => {
                // 落盘失败就放弃落盘，别让日志把主流程拖死
                eprintln!("{} [WARN ] 日志文件打不开: {e}", util::now_text());
                return;
            }
        }
    }
    if let Some((_, f)) = guard.as_mut() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\r\n");
        let _ = f.flush();
    }
}

pub fn err(msg: impl AsRef<str>) {
    write(Level::Err, msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    write(Level::Warn, msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    write(Level::Info, msg.as_ref());
}

pub fn debug(msg: impl AsRef<str>) {
    write(Level::Debug, msg.as_ref());
}

/// 读取当前日志最后 `n` 行（GUI 日志页用；M1 先提供能力，界面在 M3）
pub fn tail(n: usize) -> Vec<String> {
    let Some(p) = current_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text.lines().map(|s| s.replace('\r', "")).collect();
    if lines.len() > n {
        lines = lines.split_off(lines.len() - n);
    }
    lines
}
