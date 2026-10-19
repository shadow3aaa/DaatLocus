//! Owner-local coordination around a Windows bundle's driver and MSI transaction.
//! The cached companion CLI runs unelevated; machine trust stays in display-setup.
use super::control::{self, Action, Status};
use crate::persistence::{PersistenceFileMode, write_bytes_atomic_sync};
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Process {
    pid: u32,
    started: u64,
}
impl Process {
    fn alive(&self) -> bool {
        sysinfo::System::new_all()
            .process(sysinfo::Pid::from_u32(self.pid))
            .is_some_and(|process| process.start_time() == self.started)
    }
    fn parent() -> Result<Self> {
        let system = sysinfo::System::new_all();
        let pid = system
            .process(sysinfo::Pid::from_u32(std::process::id()))
            .and_then(|process| process.parent())
            .ok_or_else(|| miette!("Installer owner is unavailable"))?;
        let process = system
            .process(pid)
            .ok_or_else(|| miette!("Installer owner exited"))?;
        Ok(Self {
            pid: pid.as_u32(),
            started: process.start_time(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Client {
    process: Process,
    profile: Option<String>,
    background: bool,
}
#[derive(Serialize, Deserialize)]
struct Transaction {
    id: Uuid,
    owner: Process,
    installation: PathBuf,
    blocked: bool,
    services: Vec<Status>,
    clients: Vec<Client>,
}

fn journal(namespace: &Path) -> PathBuf {
    namespace.join("runtime/installation.json")
}
fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_owned());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_owned());
    #[cfg(windows)]
    {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
fn executable(installation: &Path, client: bool) -> PathBuf {
    installation.join(match (client, cfg!(windows)) {
        (true, true) => "daat-locus-app.exe",
        (true, false) => "daat-locus-app",
        (false, true) => "daat-locus.exe",
        (false, false) => "daat-locus",
    })
}
fn read(namespace: &Path) -> Result<Option<Transaction>> {
    let path = journal(namespace);
    if !path.try_exists().into_diagnostic()? {
        return Ok(None);
    }
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.readable_roots = vec![namespace.canonicalize().into_diagnostic()?];
    policy.filesystem.bound_roots = policy.filesystem.readable_roots.clone();
    let bytes = crate::file_io::read_authorized_bytes(&path, 256 * 1024, &policy)?;
    serde_json::from_slice(&bytes).into_diagnostic().map(Some)
}
fn save(namespace: &Path, transaction: &Transaction) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(transaction).into_diagnostic()?;
    if bytes.len() > 256 * 1024 {
        return Err(miette!("Installation transaction exceeds its size limit"));
    }
    write_bytes_atomic_sync(&journal(namespace), &bytes, PersistenceFileMode::Private)
        .into_diagnostic()
}

/// Held until a service publishes its control endpoint, so prepare cannot miss
/// a process between checking the installation fence and becoming discoverable.
pub async fn startup_guard(namespace: &Path) -> Result<fs::File> {
    let directory = namespace.join("runtime");
    fs::create_dir_all(&directory).into_diagnostic()?;
    crate::actors::permissions::protect_private_path(&directory)?;
    let path = directory.join("installation.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .into_diagnostic()?;
    crate::persistence::set_private_file_permissions(&path).into_diagnostic()?;
    for _ in 0..600 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(error) => return Err(miette!("Cannot lock installation lifecycle: {error}")),
        }
    }
    Err(miette!(
        "Another service startup or installer is still active"
    ))
}
pub fn check_start(namespace: &Path) -> Result<()> {
    check_executable(namespace, &std::env::current_exe().into_diagnostic()?)
}
fn check_executable(namespace: &Path, image: &Path) -> Result<()> {
    if let Some(transaction) = read(namespace)?
        && transaction.blocked
        && transaction.owner.alive()
        && same_path(image, &executable(&transaction.installation, false))
    {
        return Err(miette!(
            "Daat Locus is being updated; wait for setup to finish"
        ));
    }
    Ok(())
}

fn profiles(namespace: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = vec![namespace.to_owned()];
    let directory = namespace.join("profiles");
    if directory.try_exists().into_diagnostic()? {
        for entry in fs::read_dir(directory).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            if entry.file_type().into_diagnostic()?.is_dir() {
                roots.push(entry.path());
            }
        }
    }
    roots.sort();
    Ok(roots)
}
fn clients(installation: &Path) -> Vec<Client> {
    let target = executable(installation, true);
    sysinfo::System::new_all()
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            if !process.exe().is_some_and(|path| same_path(path, &target)) {
                return None;
            }
            let args = process.cmd();
            let profile = args
                .iter()
                .find_map(|arg| arg.to_str()?.strip_prefix("--profile=").map(str::to_owned))
                .or_else(|| {
                    args.windows(2)
                        .find(|pair| pair[0] == "--profile")
                        .map(|pair| pair[1].to_string_lossy().into_owned())
                });
            Some(Client {
                process: Process {
                    pid: pid.as_u32(),
                    started: process.start_time(),
                },
                profile,
                background: args.iter().any(|arg| arg == "--background"),
            })
        })
        .collect()
}

pub async fn prepare(namespace: &Path, installation: &Path, id: Uuid) -> Result<()> {
    if !installation.is_absolute() {
        return Err(miette!("Installation directory must be absolute"));
    }
    let guard = startup_guard(namespace).await?;
    let previous = read(namespace)?;
    if previous
        .as_ref()
        .is_some_and(|previous| previous.owner.alive())
    {
        return Err(miette!("Another installation transaction is active"));
    }
    let mut transaction = Transaction {
        id,
        owner: Process::parent()?,
        installation: installation.to_owned(),
        blocked: true,
        services: vec![],
        clients: clients(installation),
    };
    for root in profiles(namespace)? {
        if let Some(status) = control::load(&root)?
            && same_path(&status.executable, &executable(installation, false))
        {
            // Query authenticates the OS owner, instance and PID before recording a restart.
            let status = control::query(&root, Action::Status)
                .await?
                .ok_or_else(|| miette!("Service exited while preparing the upgrade"))?;
            transaction.services.push(status);
        }
    }
    if let Some(previous) = previous
        && same_path(&previous.installation, installation)
    {
        // A crashed setup must not forget the services it already stopped.
        for status in previous.services {
            if !transaction
                .services
                .iter()
                .any(|current| current.root == status.root)
            {
                transaction.services.push(status);
            }
        }
        if transaction.clients.is_empty() {
            transaction.clients = previous.clients;
        }
    }
    save(namespace, &transaction)?;
    let result = async {
        for status in &transaction.services {
            control::stop(&status.root).await?;
        }
        // Existing desktop clients exit themselves when the companion disappears.
        // Wait for their images/WebViews to close before MSI replaces files.
        for _ in 0..150 {
            if transaction
                .clients
                .iter()
                .all(|client| !client.process.alive())
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(miette!(
            "Desktop client did not exit after service shutdown; close it and retry setup"
        ))
    }
    .await;
    drop(guard);
    if let Err(error) = result {
        // Restore immediately on a prepare failure, and tolerate a subsequent
        // idempotent Burn rollback of this same step.
        if let Err(restore) = resume(namespace, installation, id).await {
            return Err(miette!(
                "{error}; restoring the service also failed: {restore}"
            ));
        }
        return Err(error);
    }
    Ok(())
}

pub async fn resume(namespace: &Path, installation: &Path, id: Uuid) -> Result<()> {
    let guard = startup_guard(namespace).await?;
    let Some(mut transaction) = read(namespace)? else {
        return Ok(());
    };
    if transaction.id != id || !same_path(&transaction.installation, installation) {
        return Err(miette!("Installation transaction does not match"));
    }
    if transaction.owner.alive() && transaction.owner != Process::parent()? {
        return Err(miette!(
            "Installation transaction belongs to another live installer"
        ));
    }
    // Driver/MSI work (or rollback) has finished. Keep the journal for retry until
    // restoration succeeds, but allow the installed companion to start again.
    transaction.blocked = false;
    save(namespace, &transaction)?;
    drop(guard);
    if let Err(error) = restore(namespace, installation, &transaction).await {
        // Burn can now roll MSI back. Do not leave newly restored images or
        // display handles alive while it replaces the installation again.
        transaction.blocked = true;
        save(namespace, &transaction)?;
        for previous in &transaction.services {
            if let Some(current) = control::load(&previous.root)?
                && current.instance != previous.instance
                && same_path(&current.executable, &executable(installation, false))
            {
                control::stop(&previous.root).await?;
            }
        }
        return Err(error);
    }
    let _guard = startup_guard(namespace).await?;
    if read(namespace)?.is_some_and(|current| current.id == id) {
        fs::remove_file(journal(namespace)).into_diagnostic()?;
    }
    Ok(())
}

async fn restore(namespace: &Path, installation: &Path, transaction: &Transaction) -> Result<()> {
    for status in &transaction.services {
        let expected = status.options.profile.as_ref().map_or_else(
            || namespace.to_owned(),
            |profile| namespace.join("profiles").join(profile),
        );
        if status.root != expected
            || status.namespace != namespace
            || !same_path(&status.executable, &executable(installation, false))
        {
            return Err(miette!(
                "Saved service does not belong to this installation/profile"
            ));
        }
        let mut command = tokio::process::Command::new(executable(installation, false));
        command
            .arg("start")
            .arg("--listen")
            .arg(status.options.listen.to_string())
            .arg("--workspace")
            .arg(&status.options.workspace)
            .env("DAAT_LOCUS_HOME", namespace)
            .env_remove("DAAT_LOCUS_PROFILE_NAME")
            .env_remove("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(profile) = &status.options.profile {
            command.arg("--profile").arg(profile);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let exit = tokio::time::timeout(Duration::from_secs(100), command.status())
            .await
            .map_err(|_| miette!("Service restoration timed out"))?
            .into_diagnostic()?;
        if !exit.success() {
            return Err(miette!(
                "Service restoration failed; inspect its profile log"
            ));
        }
    }
    for client in &transaction.clients {
        if client.process.alive() {
            continue;
        }
        let mut command = std::process::Command::new(executable(installation, true));
        command
            .env("DAAT_LOCUS_HOME", namespace)
            .env_remove("DAAT_LOCUS_PROFILE_NAME")
            .env_remove("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(profile) = &client.profile {
            command.arg("--profile").arg(profile);
        }
        if client.background {
            command.arg("--background");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.spawn().into_diagnostic()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_fence_is_scoped_to_the_live_installer_generation_and_installation() {
        let temporary = tempfile::tempdir().unwrap();
        let namespace = temporary.path().join("account");
        let _guard = startup_guard(&namespace).await.unwrap();
        let installation = temporary.path().join("installed");
        let pid = std::process::id();
        let system = sysinfo::System::new_all();
        let mut transaction = Transaction {
            id: Uuid::new_v4(),
            owner: Process {
                pid,
                started: system
                    .process(sysinfo::Pid::from_u32(pid))
                    .unwrap()
                    .start_time(),
            },
            installation: installation.clone(),
            blocked: true,
            services: vec![],
            clients: vec![],
        };
        save(&namespace, &transaction).unwrap();
        assert!(check_executable(&namespace, &executable(&installation, false)).is_err());
        assert!(
            check_executable(
                &namespace,
                &executable(&temporary.path().join("another-installation"), false)
            )
            .is_ok()
        );
        transaction.owner.started += 1;
        save(&namespace, &transaction).unwrap();
        assert!(
            check_executable(&namespace, &executable(&installation, false)).is_ok(),
            "PID reuse must not keep an old installer fence alive"
        );
        transaction.owner.started -= 1;
        transaction.blocked = false;
        save(&namespace, &transaction).unwrap();
        assert!(
            check_executable(&namespace, &executable(&installation, false)).is_ok(),
            "restoration starts only after the installer unblocks startup"
        );
    }

    #[tokio::test]
    async fn mismatched_finalization_cannot_clear_a_transaction_or_start_a_stopped_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let namespace = temporary.path().join("account");
        let installation = temporary.path().join("installed");
        let id = Uuid::new_v4();
        prepare(&namespace, &installation, id).await.unwrap();
        assert!(
            resume(&namespace, &installation, Uuid::new_v4())
                .await
                .is_err()
        );
        assert_eq!(read(&namespace).unwrap().unwrap().id, id);
        assert!(
            resume(&namespace, &temporary.path().join("other"), id)
                .await
                .is_err()
        );
        resume(&namespace, &installation, id).await.unwrap();
        assert!(!journal(&namespace).exists());
        assert!(!namespace.join("runtime/daemon.json").exists());
        resume(&namespace, &installation, id).await.unwrap();
    }
}
