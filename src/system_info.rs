use std::{fmt::Display, sync::LazyLock, time};

use parking_lot::Mutex;

static SYSTEM_SAMPLER: LazyLock<Mutex<SystemSampler>> =
    LazyLock::new(|| Mutex::new(SystemSampler::new()));

struct SystemSampler {
    sysinfo: sysinfo::System,
    networks: sysinfo::Networks,
    last_refresh: time::Instant,
}

impl SystemSampler {
    fn new() -> Self {
        let mut sysinfo = sysinfo::System::new();
        sysinfo.refresh_memory();
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let last_refresh = time::Instant::now();
        Self {
            sysinfo,
            networks,
            last_refresh,
        }
    }

    fn refresh(&mut self) {
        self.sysinfo.refresh_memory();
        self.networks.refresh(true);
        self.last_refresh = time::Instant::now();
    }

    fn sample(&mut self) -> SystemInfo {
        let elapsed = self.last_refresh.elapsed();
        let used_memory = self.sysinfo.used_memory() / 1024 / 1024; // Convert to MB
        let total_memory = self.sysinfo.total_memory() / 1024 / 1024; // Convert to MB
        let memory_usage_hundredths = used_memory
            .saturating_mul(10_000)
            .checked_div(total_memory)
            .unwrap_or(0);

        let mut total_received = 0;
        let mut total_transmitted = 0;
        for (_, data) in &self.networks {
            total_received += data.received();
            total_transmitted += data.transmitted();
        }

        let elapsed_millis = u64::try_from(elapsed.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let network_upload_speed = total_transmitted.saturating_mul(8).saturating_mul(1_000)
            / elapsed_millis
            / 1024
            / 1024;
        let network_download_speed =
            total_received.saturating_mul(8).saturating_mul(1_000) / elapsed_millis / 1024 / 1024;

        self.refresh();

        SystemInfo {
            used_memory,
            total_memory,
            memory_usage_hundredths,
            network_upload_speed,
            network_download_speed,
        }
    }
}

pub struct SystemInfo {
    /// Mb of Memory usage
    used_memory: u64,
    /// Mb of total Memory
    total_memory: u64,
    /// Percentage of memory usage, represented in hundredths of a percent.
    memory_usage_hundredths: u64,
    /// Mbps
    network_upload_speed: u64,
    /// Mbps
    network_download_speed: u64,
}

impl SystemInfo {
    pub fn sample() -> Self {
        SYSTEM_SAMPLER.lock().sample()
    }
}

impl Display for SystemInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let memory_usage_whole = self.memory_usage_hundredths / 100;
        let memory_usage_fraction = self.memory_usage_hundredths % 100;
        writeln!(
            f,
            "Memory Usage: {memory_usage_whole}.{memory_usage_fraction:02}% ({} MB / {} MB)",
            self.used_memory, self.total_memory
        )?;
        writeln!(
            f,
            "Network Upload Speed: {} Mbps",
            self.network_upload_speed
        )?;
        write!(
            f,
            "Network Download Speed: {} Mbps",
            self.network_download_speed
        )
    }
}
