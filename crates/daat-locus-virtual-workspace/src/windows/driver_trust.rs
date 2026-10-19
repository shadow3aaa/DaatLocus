//! Explicit machine trust for the one bundled self-signed display identity.
//! This is installer infrastructure, never reachable through the ComputerUse App.
use super::{DriverPackage, last_error};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    path::Path,
    ptr,
};
use windows_sys::{
    Win32::{
        Foundation::{
            CRYPT_E_NOT_FOUND, CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE,
            WAIT_ABANDONED, WAIT_OBJECT_0,
        },
        Security::{
            Cryptography::{Catalog::*, *},
            WinTrust::*,
        },
        System::{Registry::*, Threading::*},
    },
    core::{PCWSTR, w},
};

pub(crate) const CERTIFICATE: &[u8] = include_bytes!(env!("DL_DISPLAY_CERTIFICATE"));
const OWNED_VALUE: PCWSTR = w!("OwnedCertificateStores");
const PACKAGE_VALUE: PCWSTR = w!("PackageSha256");
const STORE_NAMES: [PCWSTR; 2] = [w!("Root"), w!("TrustedPublisher")];

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

struct SetupLock(HANDLE);
impl SetupLock {
    fn acquire() -> Result<Self> {
        let handle =
            unsafe { CreateMutexW(ptr::null(), 0, w!("Global\\DaatLocusVirtualDisplaySetup")) };
        if handle.is_null() {
            return Err(last_error("CreateMutexW(display setup)"));
        }
        let lock = Self(handle);
        match unsafe { WaitForSingleObject(handle, 30_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(lock),
            _ => Err(Error::InvalidState(
                "another virtual display installer is busy",
            )),
        }
    }
}
impl Drop for SetupLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

struct Certificate(*mut CERT_CONTEXT);
impl Certificate {
    fn bundled() -> Result<Self> {
        let cert = unsafe {
            CertCreateCertificateContext(
                X509_ASN_ENCODING,
                CERTIFICATE.as_ptr(),
                CERTIFICATE.len() as u32,
            )
        };
        if cert.is_null() {
            return Err(last_error("decode bundled display certificate"));
        }
        Ok(Self(cert))
    }
}
impl Drop for Certificate {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateContext(self.0);
        }
    }
}

struct Store(HCERTSTORE);
impl Store {
    fn machine(name: PCWSTR) -> Result<Self> {
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_OPEN_EXISTING_FLAG,
                name.cast(),
            )
        };
        if store.is_null() {
            return Err(last_error("open machine certificate store"));
        }
        Ok(Self(store))
    }
    fn find(&self, cert: &Certificate) -> Result<Option<Certificate>> {
        let found = unsafe {
            CertFindCertificateInStore(
                self.0,
                X509_ASN_ENCODING,
                0,
                CERT_FIND_EXISTING,
                cert.0.cast(),
                ptr::null(),
            )
        };
        if found.is_null() {
            if unsafe { windows_sys::Win32::Foundation::GetLastError() } == CRYPT_E_NOT_FOUND as u32
            {
                return Ok(None);
            }
            return Err(last_error("find bundled certificate"));
        }
        let found = Certificate(found);
        // Compare encoded bytes as well: cleanup must never target another identity.
        if unsafe {
            std::slice::from_raw_parts((*found.0).pbCertEncoded, (*found.0).cbCertEncoded as usize)
        } != CERTIFICATE
        {
            return Err(Error::InvalidState("certificate identity collision"));
        }
        Ok(Some(found))
    }
    fn add(&self) -> Result<()> {
        if unsafe {
            CertAddEncodedCertificateToStore(
                self.0,
                X509_ASN_ENCODING,
                CERTIFICATE.as_ptr(),
                CERTIFICATE.len() as u32,
                CERT_STORE_ADD_NEW,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("add bundled display certificate"));
        }
        Ok(())
    }
    fn remove(&self, cert: &Certificate) -> Result<()> {
        if let Some(found) = self.find(cert)? {
            let raw = found.0;
            std::mem::forget(found); // CertDeleteCertificateFromStore always consumes the context.
            if unsafe { CertDeleteCertificateFromStore(raw) } == 0 {
                return Err(last_error("remove bundled display certificate"));
            }
        }
        Ok(())
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        unsafe {
            CertCloseStore(self.0, 0);
        }
    }
}

struct Ledger(HKEY);
impl Ledger {
    fn open(root: HKEY, path: PCWSTR) -> Result<Self> {
        let mut key = ptr::null_mut();
        registry(unsafe {
            RegCreateKeyExW(
                root,
                path,
                0,
                ptr::null(),
                0,
                KEY_READ | KEY_WRITE | KEY_WOW64_64KEY,
                ptr::null(),
                &mut key,
                ptr::null_mut(),
            )
        })?;
        Ok(Self(key))
    }
    fn owned(&self) -> Result<u32> {
        let mut value = 0u32;
        let mut length = size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                self.0,
                ptr::null(),
                OWNED_VALUE,
                RRF_RT_REG_DWORD,
                ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut length,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(0);
        }
        registry(status)?;
        if value & !3 != 0 {
            return Err(Error::InvalidState(
                "invalid display trust ownership record",
            ));
        }
        Ok(value)
    }
    fn set_owned(&self, value: u32) -> Result<()> {
        registry(unsafe {
            RegSetValueExW(
                self.0,
                OWNED_VALUE,
                0,
                REG_DWORD,
                value.to_le_bytes().as_ptr(),
                4,
            )
        })?;
        registry(unsafe { RegFlushKey(self.0) })
    }
    fn package(&self, hash: Option<&str>) -> Result<()> {
        match hash {
            Some(hash) => {
                let value = wide(hash);
                registry(unsafe {
                    RegSetValueExW(
                        self.0,
                        PACKAGE_VALUE,
                        0,
                        REG_SZ,
                        value.as_ptr().cast(),
                        (value.len() * 2) as u32,
                    )
                })?;
            }
            None => {
                let status = unsafe { RegDeleteValueW(self.0, PACKAGE_VALUE) };
                if status != ERROR_FILE_NOT_FOUND {
                    registry(status)?;
                }
            }
        }
        registry(unsafe { RegFlushKey(self.0) })
    }
}
impl Drop for Ledger {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn registry(status: u32) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Windows {
            operation: "display installer ownership record",
            source: std::io::Error::from_raw_os_error(status as i32),
        })
    }
}

struct Trust {
    stores: [Store; 2],
    ledger: Ledger,
    cert: Certificate,
}
impl Trust {
    fn machine() -> Result<Self> {
        let identity = Sha256::digest(CERTIFICATE)
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        let key = wide(format!("SOFTWARE\\DaatLocus\\VirtualDisplay\\{identity}"));
        Ok(Self {
            stores: [
                Store::machine(STORE_NAMES[0])?,
                Store::machine(STORE_NAMES[1])?,
            ],
            ledger: Ledger::open(HKEY_LOCAL_MACHINE, key.as_ptr())?,
            cert: Certificate::bundled()?,
        })
    }
    fn provision(&self) -> Result<()> {
        for (index, store) in self.stores.iter().enumerate() {
            if store.find(&self.cert)?.is_none() {
                let before = self.ledger.owned()?;
                // Journal before importing, so a killed installer can recover ownership.
                self.ledger.set_owned(before | (1 << index))?;
                if let Err(error) = store.add() {
                    self.ledger.set_owned(before)?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn remove(&self, mask: u32) -> Result<()> {
        for (index, store) in self.stores.iter().enumerate().rev() {
            let bit = 1 << index;
            let owned = self.ledger.owned()?;
            if owned & mask & bit != 0 {
                store.remove(&self.cert)?;
                self.ledger.set_owned(owned & !bit)?;
            }
        }
        Ok(())
    }
    fn transaction<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let before = self.ledger.owned()?;
        let result = self.provision().and_then(|()| operation());
        if result.is_err() {
            self.remove(!before).map_err(|rollback| Error::Windows {
                operation: "display trust rollback",
                source: std::io::Error::other(format!(
                    "{}; rollback failed: {rollback}",
                    result.as_ref().err().unwrap()
                )),
            })?;
        }
        result
    }
}

/// Check the CAT's actual cryptographic signature against our embedded public key,
/// before adding trust. Neither a neighboring CER nor a caller's thumbprint is accepted.
fn verify_catalog_identity(path: &Path) -> Result<()> {
    let path = wide(path);
    let mut message = ptr::null_mut();
    if unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            path.as_ptr().cast(),
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut message,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_error("read display catalog signature"));
    }
    struct Message(*mut std::ffi::c_void);
    impl Drop for Message {
        fn drop(&mut self) {
            unsafe {
                CryptMsgClose(self.0);
            }
        }
    }
    let message = Message(message);
    let cert = Certificate::bundled()?;
    if unsafe {
        CryptMsgControl(
            message.0,
            0,
            CMSG_CTRL_VERIFY_SIGNATURE,
            (*cert.0).pCertInfo.cast(),
        )
    } == 0
    {
        return Err(last_error(
            "catalog must be signed by the bundled display certificate",
        ));
    }
    Ok(())
}

/// Verify the pinned CAT signature without provisioning any system trust.
pub fn verify_bundled_driver_identity(directory: &Path) -> Result<()> {
    let package = DriverPackage::open(directory)?;
    verify_catalog_identity(&package.directory.join("DaatLocusVirtualDisplay.cat"))
}

struct CatalogAdmin(isize);
impl Drop for CatalogAdmin {
    fn drop(&mut self) {
        unsafe {
            CryptCATAdminReleaseContext(self.0, 0);
        }
    }
}

/// Verify package membership and Windows Authenticode policy after explicit trust.
fn verify_member(catalog: &Path, member: &Path) -> Result<()> {
    let mut admin = 0;
    if unsafe {
        CryptCATAdminAcquireContext2(&mut admin, ptr::null(), w!("SHA256"), ptr::null(), 0)
    } == 0
    {
        return Err(last_error("open SHA256 catalog verifier"));
    }
    let admin = CatalogAdmin(admin);
    let file = fs::File::open(member).map_err(|source| Error::Windows {
        operation: "open driver package member",
        source,
    })?;
    let handle = file.as_raw_handle();
    let mut hash = [0u8; 32];
    let mut length = hash.len() as u32;
    if unsafe {
        CryptCATAdminCalcHashFromFileHandle2(admin.0, handle, &mut length, hash.as_mut_ptr(), 0)
    } == 0
    {
        return Err(last_error("hash catalog member"));
    }
    let tag = wide(hash.iter().map(|b| format!("{b:02X}")).collect::<String>());
    let catalog = wide(catalog);
    let member = wide(member);
    let mut info = WINTRUST_CATALOG_INFO {
        cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
        pcwszCatalogFilePath: catalog.as_ptr(),
        pcwszMemberTag: tag.as_ptr(),
        pcwszMemberFilePath: member.as_ptr(),
        hMemberFile: handle,
        pbCalculatedFileHash: hash.as_mut_ptr(),
        cbCalculatedFileHash: length,
        hCatAdmin: admin.0,
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        dwUnionChoice: WTD_CHOICE_CATALOG,
        Anonymous: WINTRUST_DATA_0 {
            pCatalog: &mut info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
    }
    if status != 0 {
        return Err(Error::Windows {
            operation: "verify driver catalog member",
            source: std::io::Error::from_raw_os_error(status),
        });
    }
    Ok(())
}

/// Elevated installer entry point. Only the embedded display identity is trusted.
pub fn install_bundled_driver(directory: &Path) -> Result<bool> {
    let _lock = SetupLock::acquire()?;
    let package = DriverPackage::open(directory)?;
    let catalog = package.directory.join("DaatLocusVirtualDisplay.cat");
    // Keep source bytes stable across signature verification and PnP staging.
    let _files: Vec<_> = [
        "DaatLocusVirtualDisplay.cat",
        "DaatLocusVirtualDisplay.inf",
        "DaatLocusVirtualDisplay.dll",
    ]
    .iter()
    .map(|name| {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(package.directory.join(name))
    })
    .collect::<std::io::Result<_>>()
    .map_err(|source| Error::Windows {
        operation: "lock driver package files",
        source,
    })?;
    verify_catalog_identity(&catalog)?;
    let bytes = fs::read(&catalog).map_err(|source| Error::Windows {
        operation: "read driver catalog",
        source,
    })?;
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let previous: Vec<_> = installed_packages()?
        .into_iter()
        .map(|package| package.directory)
        .collect();
    let trust = Trust::machine()?;
    let owned_before = trust.ledger.owned()?;
    trust.transaction(|| {
        for name in ["DaatLocusVirtualDisplay.inf", "DaatLocusVirtualDisplay.dll"] {
            verify_member(&catalog, &package.directory.join(name))?;
        }
        Ok(())
    })?;
    let reboot = match package.install() {
        Ok(reboot) => reboot,
        Err(install_error) => {
            let rollback = (|| -> Result<()> {
                let mut reboot = false;
                for staged in installed_packages()? {
                    if !previous.contains(&staged.directory) {
                        reboot |= staged.uninstall()?;
                    }
                }
                if reboot {
                    return Err(Error::InvalidState(
                        "restart required before removing display trust; rerun uninstall after restarting",
                    ));
                }
                trust.remove(!owned_before)
            })();
            if let Err(rollback) = rollback {
                return Err(Error::Windows {
                    operation: "display installation rollback",
                    source: std::io::Error::other(format!(
                        "{install_error}; trust retained: {rollback}"
                    )),
                });
            }
            return Err(install_error);
        }
    };
    // A detection-record failure must not revoke trust from a successfully bound driver.
    if let Err(error) = trust.ledger.package(Some(&hash)) {
        eprintln!("driver installed; detection record unavailable: {error}");
    }
    Ok(reboot)
}

/// Explicit removal of the shared machine driver; per-user client uninstall leaves it.
/// Remove every stored version signed by this identity before revoking owned trust.
pub fn uninstall_bundled_driver() -> Result<bool> {
    let _lock = SetupLock::acquire()?;
    let packages = installed_packages()?;
    let mut reboot = super::driver_install::remove_driver_adapters()?;
    for package in packages {
        reboot |= package.uninstall()?;
    }
    let trust = Trust::machine()?;
    // Keep trust until a required restart completes. A repeated uninstall finishes cleanup.
    trust.ledger.package(None)?;
    if !reboot {
        trust.remove(3)?;
    }
    Ok(reboot)
}

fn installed_packages() -> Result<Vec<DriverPackage>> {
    let mut windows = [0u16; 32768];
    let count = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            windows.as_mut_ptr(),
            windows.len() as u32,
        )
    } as usize;
    if count == 0 || count >= windows.len() {
        return Err(last_error("GetWindowsDirectoryW"));
    }
    let directory = std::path::PathBuf::from(std::ffi::OsString::from_wide(&windows[..count]))
        .join("System32/DriverStore/FileRepository");
    let mut packages = Vec::new();
    for entry in fs::read_dir(directory).map_err(|source| Error::Windows {
        operation: "enumerate installed display packages",
        source,
    })? {
        let entry = entry.map_err(|source| Error::Windows {
            operation: "enumerate installed display package",
            source,
        })?;
        if !entry
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("daatlocusvirtualdisplay.inf_")
        {
            continue;
        }
        let path = entry.path();
        if verify_catalog_identity(&path.join("DaatLocusVirtualDisplay.cat")).is_ok() {
            packages.push(DriverPackage::open(path)?);
        }
    }
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        trust: Option<Trust>,
        path: Vec<u16>,
    }
    impl Fixture {
        fn new() -> Self {
            let path = wide(format!(
                "Software\\DaatLocusTests\\{}",
                uuid::Uuid::new_v4()
            ));
            let memory = || {
                let handle = unsafe { CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, ptr::null()) };
                assert!(!handle.is_null());
                Store(handle)
            };
            Self {
                trust: Some(Trust {
                    stores: [memory(), memory()],
                    ledger: Ledger::open(HKEY_CURRENT_USER, path.as_ptr()).unwrap(),
                    cert: Certificate::bundled().unwrap(),
                }),
                path,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.trust.take();
            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, self.path.as_ptr());
            }
        }
    }

    #[test]
    fn rollback_and_uninstall_preserve_preexisting_trust() {
        for preexisting in [0, 1, 2, 3] {
            let fixture = Fixture::new();
            let trust = fixture.trust.as_ref().unwrap();
            for i in 0..2 {
                if preexisting & (1 << i) != 0 {
                    trust.stores[i].add().unwrap();
                }
            }
            let failed: Result<()> =
                trust.transaction(|| Err(Error::InvalidState("installation failed")));
            assert!(failed.is_err());
            assert_eq!(trust.ledger.owned().unwrap(), 0);
            for i in 0..2 {
                assert_eq!(
                    trust.stores[i].find(&trust.cert).unwrap().is_some(),
                    preexisting & (1 << i) != 0
                );
            }
            trust.transaction(|| Ok(())).unwrap();
            assert_eq!(trust.ledger.owned().unwrap(), 3 & !preexisting);
            // Repair and a subsequent failed upgrade retain ownership of earlier imports.
            trust.transaction(|| Ok(())).unwrap();
            let _: Result<()> = trust.transaction(|| Err(Error::InvalidState("upgrade failed")));
            assert!(
                trust
                    .stores
                    .iter()
                    .all(|store| store.find(&trust.cert).unwrap().is_some())
            );
            trust.remove(3).unwrap();
            trust.remove(3).unwrap();
            for i in 0..2 {
                assert_eq!(
                    trust.stores[i].find(&trust.cert).unwrap().is_some(),
                    preexisting & (1 << i) != 0
                );
            }
        }
    }

    #[test]
    fn partial_import_failure_rolls_back_only_new_entries() {
        let mut fixture = Fixture::new();
        let trust = fixture.trust.as_mut().unwrap();
        // A collection with no writable sibling exercises a real CryptoAPI add failure.
        let readonly = unsafe { CertOpenStore(CERT_STORE_PROV_COLLECTION, 0, 0, 0, ptr::null()) };
        assert!(!readonly.is_null());
        trust.stores[1] = Store(readonly);
        let result: Result<()> = trust.transaction(|| panic!("driver installation must not start"));
        assert!(result.is_err());
        assert!(trust.stores[0].find(&trust.cert).unwrap().is_none());
        assert_eq!(trust.ledger.owned().unwrap(), 0);
    }

    #[test]
    #[ignore = "requires elevation, no bundled display certificate/package installed, and DAAT_LOCUS_SIGNED_IDD_PACKAGE; installs and removes the shared DL driver"]
    fn native_bundled_installation_rejects_tampering_and_cleans_trust() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("DAAT_LOCUS_SIGNED_IDD_PACKAGE")
                .expect("set DAAT_LOCUS_SIGNED_IDD_PACKAGE to the signed distribution package"),
        );
        let trust = Trust::machine().expect("run this explicit driver test as administrator");
        assert!(
            installed_packages().unwrap().is_empty(),
            "remove the bundled driver before this test"
        );
        assert_eq!(trust.ledger.owned().unwrap(), 0);
        assert!(
            trust
                .stores
                .iter()
                .all(|store| store.find(&trust.cert).unwrap().is_none()),
            "use a test machine without this certificate"
        );
        verify_bundled_driver_identity(&directory).unwrap();
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "DaatLocusVirtualDisplay.inf",
            "DaatLocusVirtualDisplay.dll",
            "DaatLocusVirtualDisplay.cat",
        ] {
            fs::copy(directory.join(name), temp.path().join(name)).unwrap();
        }
        let inf = temp.path().join("DaatLocusVirtualDisplay.inf");
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(inf)
            .unwrap()
            .write_all(b"\r\n; altered after signing\r\n")
            .unwrap();
        assert!(
            install_bundled_driver(temp.path()).is_err(),
            "modified catalog member must be rejected"
        );
        assert!(installed_packages().unwrap().is_empty());
        assert_eq!(trust.ledger.owned().unwrap(), 0);
        assert!(
            trust
                .stores
                .iter()
                .all(|store| store.find(&trust.cert).unwrap().is_none())
        );
        install_bundled_driver(&directory).unwrap();
        install_bundled_driver(&directory).unwrap();
        // System stores cache per opened handle; observe through fresh handles,
        // as a subsequent installer invocation would.
        let trust = Trust::machine().unwrap();
        assert_eq!(trust.ledger.owned().unwrap(), 3);
        assert!(
            trust
                .stores
                .iter()
                .all(|store| store.find(&trust.cert).unwrap().is_some())
        );
        let screen = super::super::VirtualDisplay::create(Default::default()).unwrap();
        assert_eq!(screen.display().unwrap().bounds.width().unwrap(), 1920);
        screen.remove().unwrap();
        assert!(
            !uninstall_bundled_driver().unwrap(),
            "restart and finish cleanup if required"
        );
        let trust = Trust::machine().unwrap();
        assert!(installed_packages().unwrap().is_empty());
        assert_eq!(trust.ledger.owned().unwrap(), 0);
        assert!(
            trust
                .stores
                .iter()
                .all(|store| store.find(&trust.cert).unwrap().is_none())
        );
    }
}
