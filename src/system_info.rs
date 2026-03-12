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
        self.sysinfo.refresh_cpu_all();
        self.sysinfo.refresh_memory();
        self.networks.refresh(true);
        self.last_refresh = time::Instant::now();
    }

    fn sample(&mut self) -> SystemInfo {
        let elapsed = self.last_refresh.elapsed();
        let elapsed_secs = elapsed.as_secs_f64().max(0.001); // Avoid division by zero
        let cpu_usage = self.sysinfo.global_cpu_usage();
        let used_memory = self.sysinfo.used_memory() / 1024 / 1024; // Convert to MB
        let total_memory = self.sysinfo.total_memory() / 1024 / 1024; // Convert to MB
        let memory_usage = used_memory as f32 / total_memory as f32 * 100.0; // Percentage

        let mut total_received = 0;
        let mut total_transmitted = 0;
        for (_, data) in &self.networks {
            total_received += data.received();
            total_transmitted += data.transmitted();
        }

        let network_upload_speed =
            ((total_transmitted as f64 / elapsed_secs) / 1024.0 / 1024.0 * 8.0) as u64;
        let network_download_speed =
            ((total_received as f64 / elapsed_secs) / 1024.0 / 1024.0 * 8.0) as u64;

        self.refresh();

        SystemInfo {
            cpu_usage,
            used_memory,
            total_memory,
            memory_usage,
            network_upload_speed,
            network_download_speed,
        }
    }
}

pub struct SystemInfo {
    /// Percentage of CPU usage
    cpu_usage: f32,
    /// Mb of Memory usage
    used_memory: u64,
    /// Mb of total Memory
    total_memory: u64,
    /// Percentage of Memory usage
    memory_usage: f32,
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
        writeln!(f, "CPU Usage: {:.2}%", self.cpu_usage)?;
        writeln!(
            f,
            "Memory Usage: {:.2}% ({} MB / {} MB)",
            self.memory_usage, self.used_memory, self.total_memory
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
