//! 文件变更监控（文档 §5.9）：`notify` 封装，事件溢出时由上层降级为目录重扫。

use notify::{Event, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub struct WatchHandle {
    _watcher: notify::RecommendedWatcher,
}

/// 启动递归监控；事件批量转发到 `tx`，由服务层做 1s 去抖与合并。
pub fn start(roots: &[PathBuf], tx: Sender<Vec<PathBuf>>) -> notify::Result<WatchHandle> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            let mut paths: Vec<PathBuf> = Vec::new();
            for p in event.paths {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
            if !paths.is_empty() {
                let _ = tx.send(paths);
            }
        }
        Err(_) => {
            // 事件通道异常：通知上层做一次全量重扫
            let _ = tx.send(Vec::new());
        }
    })?;
    // 逐个根目录注册：部分失败（权限/网络盘掉线）仍可监听其余根部；
    // 但**一个都注册不上**时必须把错误交回调用方——否则上层会以为
    // 「监控中」，实际永远收不到变更事件。
    let mut watched = 0usize;
    let mut last_err: Option<notify::Error> = None;
    for root in roots {
        match watcher.watch(root, RecursiveMode::Recursive) {
            Ok(()) => watched += 1,
            Err(e) => last_err = Some(e),
        }
    }
    if watched == 0 {
        if let Some(e) = last_err {
            return Err(e);
        }
    }
    Ok(WatchHandle { _watcher: watcher })
}
