use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const TARGET_NOFILE: u64 = 262_144;

pub fn raise_nofile_limit() {
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            let target = (TARGET_NOFILE as libc::rlim_t).min(limit.rlim_max);
            if limit.rlim_cur < target {
                limit.rlim_cur = target;
                let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
        }
    }
}

pub struct ProxyCapacity {
    pub(crate) ingress: Arc<Semaphore>,
    pub(crate) meter: Arc<Semaphore>,
    limit: usize,
    dropped: AtomicU64,
    last_warn_at: AtomicU64,
}

impl ProxyCapacity {
    pub(super) fn for_node() -> Self {
        let sysconf = |key| {
            u64::try_from(unsafe { libc::sysconf(key) }).ok().filter(|n| *n > 0)
        };
        let memory = sysconf(libc::_SC_PHYS_PAGES)
            .zip(sysconf(libc::_SC_PAGESIZE))
            .and_then(|(pages, size)| pages.checked_mul(size));
        #[cfg(target_os = "linux")]
        let memory = {
            let limit = std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|groups| {
                cgroup_memory_limit(&groups, std::path::Path::new("/sys/fs/cgroup"))
            });
            memory.into_iter().chain(limit).min()
        };
        Self::new(connection_budget(sysconf(libc::_SC_OPEN_MAX), memory))
    }

    pub(super) fn new(limit: usize) -> Self {
        // The public listener may feed the meter. Reserve capacity for both stages independently.
        Self {
            ingress: Arc::new(Semaphore::new(limit)),
            meter: Arc::new(Semaphore::new(limit)),
            limit,
            dropped: AtomicU64::new(0),
            last_warn_at: AtomicU64::new(0),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn acquire_ingress(&self, stage: &str) -> Option<OwnedSemaphorePermit> {
        self.acquire(&self.ingress, stage)
    }

    pub(crate) fn acquire_meter(&self, stage: &str) -> Option<OwnedSemaphorePermit> {
        self.acquire(&self.meter, stage)
    }

    fn acquire(&self, semaphore: &Arc<Semaphore>, stage: &str) -> Option<OwnedSemaphorePermit> {
        match semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                let now = crate::model::now();
                let previous = self.last_warn_at.load(Ordering::Relaxed);
                if now.saturating_sub(previous) >= 10
                    && self
                        .last_warn_at
                        .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                {
                    eprintln!(
                        "节点连接保护额度已满（上限 {}，累计拒绝 {}）：{}",
                        self.limit, total, stage
                    );
                }
                None
            }
        }
    }
}

fn connection_budget(files: Option<u64>, memory: Option<u64>) -> usize {
    // Keep a quarter of the descriptors in reserve. A UDP association can hold four per stage.
    let fd_budget = files.map(|n| (n - n / 4) / 8).unwrap_or(1024);
    // Leave room for the cores' retained heaps and kernel buffers when successive waves overlap.
    let memory_budget = memory.map(|n| (n - n / 4) / (512 * 1024)).unwrap_or(1024);
    fd_budget.min(memory_budget).clamp(1, Semaphore::MAX_PERMITS as u64) as usize
}

#[cfg(target_os = "linux")]
fn cgroup_memory_limit(membership: &str, mount: &std::path::Path) -> Option<u64> {
    use std::path::{Component, Path};

    let mut limit = None;
    for line in membership.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(controllers), Some(path)) = (fields.nth(1), fields.next()) else {
            continue;
        };
        let (root, file) = if controllers.is_empty() {
            (mount.to_owned(), "memory.max")
        } else if controllers.split(',').any(|c| c == "memory") {
            (mount.join("memory"), "memory.limit_in_bytes")
        } else {
            continue;
        };
        let relative = Path::new(path.trim_start_matches('/'));
        let relative = if relative.components().all(|c| matches!(c, Component::Normal(_))) {
            relative
        } else {
            Path::new("")
        };
        // A parent slice can impose a tighter limit than the service's own cgroup.
        for ancestor in relative.ancestors() {
            let bytes = std::fs::read_to_string(root.join(ancestor).join(file))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            limit = limit.into_iter().chain(bytes).min();
        }
    }
    limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_reserve_descriptors_and_respect_small_nodes() {
        assert_eq!(connection_budget(Some(65_536), Some(2 << 30)), 3072);
        assert_eq!(connection_budget(Some(262_144), Some(1300 << 20)), 1950);
        assert_eq!(connection_budget(Some(1024), Some(2 << 30)), 96);
        assert_eq!(connection_budget(Some(262_144), Some(16 << 30)), 24_576);
        assert_eq!(connection_budget(None, None), 1024);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_memory_respects_parent_limits_and_unlimited_children() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path();
        std::fs::create_dir_all(mount.join("slice/agent")).unwrap();
        std::fs::write(mount.join("slice/memory.max"), "1073741824\n").unwrap();
        std::fs::write(mount.join("slice/agent/memory.max"), "max\n").unwrap();
        assert_eq!(cgroup_memory_limit("0::/slice/agent", mount), Some(1 << 30));
        std::fs::create_dir_all(mount.join("memory/agent")).unwrap();
        std::fs::write(mount.join("memory/agent/memory.limit_in_bytes"), "536870912").unwrap();
        assert_eq!(cgroup_memory_limit("3:memory:/agent", mount), Some(1 << 29));
        assert_eq!(cgroup_memory_limit("1:cpu:/agent", mount), None);
    }
}
