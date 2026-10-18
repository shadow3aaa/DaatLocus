//! Device-owned keys and bounded, verifiable enrollment chains. No ambient login.
use super::{Result, StageError, StageId, digest};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::Path,
};

const MAX_PROOF: usize = 8192;
pub const MAX_CHAIN: usize = 8;
/// Independent device clocks need not agree to the millisecond. This only
/// bounds future issuance; expiry, revocation and chain authority stay exact.
pub(crate) const CLOCK_SKEW_MS: i64 = 30_000;
const DOMAIN: &[u8] = b"DaatLocus.Stage.v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    pub stage_id: StageId,
    pub public_key: String,
    pub tls_sha256: String,
}
impl DeviceProfile {
    pub fn validate(&self) -> Result<()> {
        if self.stage_id.0.is_nil()
            || self.tls_sha256.len() != 64
            || hex::decode(&self.tls_sha256).is_err()
        {
            return Err(StageError::Invalid("Invalid device identity"));
        }
        verifying_key(&self.public_key)?;
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRoot {
    pub group_id: String,
    pub public_key: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedProof {
    pub payload: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub version: u16,
    pub group_id: String,
    pub issuer: Option<StageId>,
    pub device: DeviceProfile,
    pub join_id: uuid::Uuid,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub can_enroll: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    pub version: u16,
    pub device: DeviceProfile,
    pub nonce: uuid::Uuid,
    pub created_at_ms: i64,
    pub proof: SignedProof,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBody {
    version: u16,
    device: DeviceProfile,
    nonce: uuid::Uuid,
    created_at_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Enrollment {
    pub root: TrustRoot,
    pub chain: Vec<SignedProof>,
}

// Deliberately neither Debug nor Serialize: only explicit private storage exports secrets.
pub struct DeviceIdentity {
    stage_id: StageId,
    key: SigningKey,
    tls_certificate: Vec<u8>,
    tls_private_key: Vec<u8>,
    group_authority: Option<SigningKey>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    version: u16,
    stage_id: StageId,
    seed: String,
    tls_certificate: String,
    tls_private_key: String,
    #[serde(default)]
    group_authority_seed: Option<String>,
}
fn decode(value: &str, max: usize) -> Result<Vec<u8>> {
    if value.len() > max * 2 {
        return Err(StageError::Invalid("Proof exceeds its budget"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| StageError::Invalid("Invalid proof encoding"))?;
    if bytes.len() > max {
        return Err(StageError::Invalid("Proof exceeds its budget"));
    }
    Ok(bytes)
}
fn verifying_key(value: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = decode(value, 32)?
        .try_into()
        .map_err(|_| StageError::Invalid("Invalid device key"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| StageError::Invalid("Invalid device key"))
}
fn signed_bytes(purpose: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DOMAIN.len() + purpose.len() + 1 + payload.len());
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(purpose.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(payload);
    bytes
}
impl SignedProof {
    pub fn verify<T: DeserializeOwned>(&self, purpose: &str, key: &str) -> Result<T> {
        let bytes = decode(&self.payload, MAX_PROOF)?;
        let signature = Signature::from_slice(&decode(&self.signature, 64)?)
            .map_err(|_| StageError::Invalid("Invalid signature"))?;
        // Cache only immutable signature verification. Membership expiry,
        // revocation and Actor grants are still checked at every operation.
        static VERIFIED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        let fingerprint = super::digest(
            &serde_json::to_vec(&(purpose, key, &self.payload, &self.signature))
                .map_err(|_| StageError::Invalid("Invalid proof fingerprint"))?,
        );
        let cached = VERIFIED
            .lock()
            .map_err(|_| StageError::Unavailable("Signature cache is unavailable"))?
            .contains(&fingerprint);
        if !cached {
            verifying_key(key)?
                .verify_strict(&signed_bytes(purpose, &bytes), &signature)
                .map_err(|_| StageError::Forbidden("Invalid device signature"))?;
            let mut cache = VERIFIED
                .lock()
                .map_err(|_| StageError::Unavailable("Signature cache is unavailable"))?;
            if cache.len() >= 512 {
                cache.clear();
            }
            cache.insert(fingerprint);
        }
        serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid signed record"))
    }
}
impl DeviceIdentity {
    /// Only call from explicit Owner initialization or disposable test fixtures.
    pub fn generate() -> Result<Self> {
        Self::generate_for_stage(StageId::new())
    }
    pub fn generate_for_stage(stage_id: StageId) -> Result<Self> {
        if stage_id.0.is_nil() {
            return Err(StageError::Invalid("Invalid Stage identity"));
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)
            .map_err(|_| StageError::Unavailable("OS random source unavailable"))?;
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        let generated = rcgen::generate_simple_self_signed(vec![format!("{stage_id}.dl.invalid")])
            .map_err(|_| StageError::Unavailable("TLS certificate generation failed"))?;
        Ok(Self {
            stage_id,
            key,
            tls_certificate: generated.cert.der().to_vec(),
            tls_private_key: generated.signing_key.serialize_der(),
            group_authority: None,
        })
    }
    pub fn profile(&self) -> DeviceProfile {
        DeviceProfile {
            stage_id: self.stage_id,
            public_key: URL_SAFE_NO_PAD.encode(self.key.verifying_key().to_bytes()),
            tls_sha256: digest(&self.tls_certificate),
        }
    }
    pub fn tls_certificate(&self) -> &[u8] {
        &self.tls_certificate
    }
    pub(crate) fn tls_private_key(&self) -> &[u8] {
        &self.tls_private_key
    }
    pub fn sign<T: Serialize>(&self, purpose: &str, value: &T) -> Result<SignedProof> {
        sign_proof(&self.key, purpose, value)
    }
    pub fn join_request(&self, now: i64) -> Result<JoinRequest> {
        let body = JoinBody {
            version: 1,
            device: self.profile(),
            nonce: uuid::Uuid::new_v4(),
            created_at_ms: now,
        };
        Ok(JoinRequest {
            version: body.version,
            device: body.device.clone(),
            nonce: body.nonce,
            created_at_ms: now,
            proof: self.sign("join-request", &body)?,
        })
    }
    pub fn initialize_group(&mut self, now: i64, expires: i64) -> Result<Enrollment> {
        if expires <= now {
            return Err(StageError::Invalid(
                "Membership expiry must be in the future",
            ));
        }
        if self.group_authority.is_none() {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed)
                .map_err(|_| StageError::Unavailable("OS random source unavailable"))?;
            self.group_authority = Some(SigningKey::from_bytes(&seed));
            seed.fill(0);
        }
        let authority = self
            .group_authority
            .as_ref()
            .ok_or(StageError::Unavailable("Group authority unavailable"))?;
        let root = TrustRoot {
            group_id: digest(&authority.verifying_key().to_bytes()),
            public_key: URL_SAFE_NO_PAD.encode(authority.verifying_key().to_bytes()),
        };
        let member = Membership {
            version: 1,
            group_id: root.group_id.clone(),
            issuer: None,
            device: self.profile(),
            join_id: uuid::Uuid::new_v4(),
            issued_at_ms: now,
            expires_at_ms: expires,
            can_enroll: true,
        };
        Ok(Enrollment {
            root,
            chain: vec![sign_proof(authority, "membership", &member)?],
        })
    }
    pub fn authorize(
        &self,
        own: &Enrollment,
        request: &JoinRequest,
        now: i64,
        expires: i64,
        can_enroll: bool,
    ) -> Result<Enrollment> {
        let issuer = verify_enrollment(&own.root, &own.chain, now, &HashSet::new())?;
        if issuer.device != self.profile() || !issuer.can_enroll {
            return Err(StageError::Forbidden("Device cannot authorize enrollment"));
        }
        if own.chain.len() >= MAX_CHAIN || expires <= now || expires > issuer.expires_at_ms {
            return Err(StageError::Invalid("Invalid enrollment lifetime or chain"));
        }
        verify_join_request(request, now)?;
        let member = Membership {
            version: 1,
            group_id: own.root.group_id.clone(),
            issuer: Some(issuer.device.stage_id),
            device: request.device.clone(),
            join_id: request.nonce,
            issued_at_ms: now,
            expires_at_ms: expires,
            can_enroll,
        };
        let mut chain = own.chain.clone();
        chain.push(self.sign("membership", &member)?);
        let enrolled = Enrollment {
            root: own.root.clone(),
            chain,
        };
        verify_enrollment(&own.root, &enrolled.chain, now, &HashSet::new())?;
        Ok(enrolled)
    }
    pub fn save_new(&self, path: &Path) -> Result<()> {
        use std::fs::OpenOptions;
        let parent = path
            .parent()
            .ok_or(StageError::Invalid("Identity needs an explicit directory"))?;
        std::fs::create_dir_all(parent)?;
        crate::actors::permissions::protect_private_path(parent)
            .map_err(|_| StageError::Forbidden("Cannot establish private identity storage"))?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        crate::persistence::set_private_file_permissions(path)?;
        let stored = StoredIdentity {
            version: 1,
            stage_id: self.stage_id,
            seed: URL_SAFE_NO_PAD.encode(self.key.to_bytes()),
            tls_certificate: URL_SAFE_NO_PAD.encode(&self.tls_certificate),
            tls_private_key: URL_SAFE_NO_PAD.encode(&self.tls_private_key),
            group_authority_seed: self
                .group_authority
                .as_ref()
                .map(|key| URL_SAFE_NO_PAD.encode(key.to_bytes())),
        };
        let bytes = serde_json::to_vec(&stored)
            .map_err(|_| StageError::Invalid("Cannot encode private identity"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }
    pub fn save_existing(&self, path: &Path) -> Result<()> {
        let stored = StoredIdentity {
            version: 1,
            stage_id: self.stage_id,
            seed: URL_SAFE_NO_PAD.encode(self.key.to_bytes()),
            tls_certificate: URL_SAFE_NO_PAD.encode(&self.tls_certificate),
            tls_private_key: URL_SAFE_NO_PAD.encode(&self.tls_private_key),
            group_authority_seed: self
                .group_authority
                .as_ref()
                .map(|key| URL_SAFE_NO_PAD.encode(key.to_bytes())),
        };
        let bytes = serde_json::to_vec(&stored)
            .map_err(|_| StageError::Invalid("Cannot encode private identity"))?;
        crate::persistence::write_bytes_atomic_sync(
            path,
            &bytes,
            crate::persistence::PersistenceFileMode::Private,
        )
        .map_err(|_| StageError::Forbidden("Cannot persist private identity"))
    }
    pub fn has_group_authority(&self) -> bool {
        self.group_authority.is_some()
    }
    pub fn load(path: &Path) -> Result<Self> {
        crate::persistence::set_private_file_permissions(path)?;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(StageError::Invalid("Identity record exceeds its budget"));
        }
        let stored: StoredIdentity = serde_json::from_slice(&bytes).map_err(|_| {
            StageError::Invalid("Invalid private identity; automatic regeneration is forbidden")
        })?;
        if stored.version != 1 {
            return Err(StageError::Invalid("Unsupported identity version"));
        }
        let parse_seed = |value: &str| -> Result<SigningKey> {
            let bytes: [u8; 32] = decode(value, 32)?
                .try_into()
                .map_err(|_| StageError::Invalid("Invalid identity seed"))?;
            Ok(SigningKey::from_bytes(&bytes))
        };
        Ok(Self {
            stage_id: stored.stage_id,
            key: parse_seed(&stored.seed)?,
            tls_certificate: decode(&stored.tls_certificate, 16384)?,
            tls_private_key: decode(&stored.tls_private_key, 16384)?,
            group_authority: stored
                .group_authority_seed
                .as_deref()
                .map(parse_seed)
                .transpose()?,
        })
    }
}

fn sign_proof<T: Serialize>(key: &SigningKey, purpose: &str, value: &T) -> Result<SignedProof> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| StageError::Invalid("Cannot encode signed record"))?;
    if bytes.len() > MAX_PROOF {
        return Err(StageError::Invalid("Signed record exceeds its budget"));
    }
    Ok(SignedProof {
        signature: URL_SAFE_NO_PAD.encode(key.sign(&signed_bytes(purpose, &bytes)).to_bytes()),
        payload: URL_SAFE_NO_PAD.encode(bytes),
    })
}

pub fn verify_join_request(request: &JoinRequest, now: i64) -> Result<()> {
    request.device.validate()?;
    let body: JoinBody = request
        .proof
        .verify("join-request", &request.device.public_key)?;
    if body.version != 1
        || body.device != request.device
        || body.nonce != request.nonce
        || body.created_at_ms != request.created_at_ms
        || request.created_at_ms > now.saturating_add(CLOCK_SKEW_MS)
        || now.saturating_sub(request.created_at_ms) > 5 * 60 * 1000
    {
        return Err(StageError::Forbidden(
            "Join request is inconsistent or expired",
        ));
    }
    if request.device.tls_sha256.len() != 64 || hex::decode(&request.device.tls_sha256).is_err() {
        return Err(StageError::Invalid("Invalid TLS binding"));
    }
    Ok(())
}
/// `root` comes from local Owner trust, never from the peer's submitted bundle.
pub fn verify_enrollment(
    root: &TrustRoot,
    chain: &[SignedProof],
    now: i64,
    revoked: &HashSet<StageId>,
) -> Result<Membership> {
    if chain.is_empty()
        || chain.len() > MAX_CHAIN
        || root.group_id != digest(&verifying_key(&root.public_key)?.to_bytes())
    {
        return Err(StageError::Forbidden("Untrusted group or enrollment chain"));
    }
    let mut issuer = None;
    let mut key = root.public_key.clone();
    let mut maximum_expiry = i64::MAX;
    let mut can_enroll = true;
    let mut seen = HashSet::new();
    let mut last = None;
    for (index, proof) in chain.iter().enumerate() {
        let member: Membership = proof.verify("membership", &key)?;
        if member.version != 1
            || member.group_id != root.group_id
            || member.issuer != issuer
            || !can_enroll
            || member.issued_at_ms > now.saturating_add(CLOCK_SKEW_MS)
            || member.expires_at_ms <= now
            || member.expires_at_ms > maximum_expiry
            || revoked.contains(&member.device.stage_id)
            || !seen.insert(member.device.stage_id)
        {
            return Err(StageError::Forbidden(
                "Enrollment is expired, revoked or outside authority",
            ));
        }
        if index > 0 && member.device.public_key == root.public_key {
            return Err(StageError::Forbidden(
                "A device cannot impersonate group authority",
            ));
        }
        member.device.validate()?;
        if member.device.tls_sha256.len() != 64 || hex::decode(&member.device.tls_sha256).is_err() {
            return Err(StageError::Invalid("Invalid TLS binding"));
        }
        issuer = Some(member.device.stage_id);
        key = member.device.public_key.clone();
        maximum_expiry = member.expires_at_ms;
        can_enroll = member.can_enroll;
        last = Some(member);
    }
    last.ok_or(StageError::Forbidden("Empty enrollment chain"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_join_and_enrollment_tolerate_only_bounded_clock_skew() {
        let issued = 1_000_000;
        let mut authority = DeviceIdentity::generate().unwrap();
        let device = DeviceIdentity::generate().unwrap();
        let root = authority
            .initialize_group(issued, issued + 600_000)
            .unwrap();
        let request = device.join_request(issued).unwrap();
        let enrollment = authority
            .authorize(&root, &request, issued, issued + 300_000, true)
            .unwrap();
        for clock in [issued - 30_000, issued - 1_000, issued, issued + 1_000] {
            verify_join_request(&request, clock).unwrap();
            assert_eq!(
                verify_enrollment(&root.root, &enrollment.chain, clock, &HashSet::new())
                    .unwrap()
                    .device,
                device.profile()
            );
        }
        for clock in [issued - 30_001, issued + 300_001] {
            assert!(verify_join_request(&request, clock).is_err());
            assert!(
                verify_enrollment(&root.root, &enrollment.chain, clock, &HashSet::new()).is_err()
            );
        }
        assert!(
            verify_enrollment(
                &root.root,
                &enrollment.chain,
                issued + 300_000,
                &HashSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn independent_keys_verify_enrollment_and_reject_forgery_wrong_group_and_expiry() {
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut c = DeviceIdentity::generate().unwrap();
        assert_ne!(a.profile().public_key, b.profile().public_key);
        let root = a.initialize_group(1000, 100000).unwrap();
        let request = b.join_request(1000).unwrap();
        let enrolled = a.authorize(&root, &request, 1001, 50000, false).unwrap();
        let member = verify_enrollment(&root.root, &enrolled.chain, 1002, &HashSet::new()).unwrap();
        assert_eq!(member.device, b.profile());
        assert!(
            b.authorize(
                &enrolled,
                &c.join_request(1002).unwrap(),
                1003,
                40000,
                false
            )
            .is_err()
        );
        assert!(
            verify_enrollment(
                &c.initialize_group(1000, 100000).unwrap().root,
                &enrolled.chain,
                1002,
                &HashSet::new()
            )
            .is_err()
        );
        assert!(verify_enrollment(&root.root, &enrolled.chain, 50000, &HashSet::new()).is_err());
        assert!(
            verify_enrollment(
                &root.root,
                &enrolled.chain,
                1002,
                &HashSet::from([b.profile().stage_id])
            )
            .is_err()
        );
        let mut forged = enrolled.chain.clone();
        forged[1].signature = URL_SAFE_NO_PAD.encode([0; 64]);
        assert!(verify_enrollment(&root.root, &forged, 1002, &HashSet::new()).is_err());
        let mut swapped = request;
        swapped.device = c.profile();
        assert!(verify_join_request(&swapped, 1002).is_err());
    }
    #[test]
    fn authorized_device_can_join_another_without_copying_the_group_private_key() {
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let c = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap();
        let b_member = a
            .authorize(&root, &b.join_request(1000).unwrap(), 1001, 90000, true)
            .unwrap();
        let c_member = b
            .authorize(
                &b_member,
                &c.join_request(1001).unwrap(),
                1002,
                80000,
                false,
            )
            .unwrap();
        assert_eq!(
            verify_enrollment(&root.root, &c_member.chain, 1003, &HashSet::new())
                .unwrap()
                .device,
            c.profile()
        );
        assert!(
            verify_enrollment(
                &root.root,
                &c_member.chain,
                1003,
                &HashSet::from([b.profile().stage_id])
            )
            .is_err()
        );
    }
    #[test]
    fn private_identity_survives_reload_and_cannot_be_silently_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stage/identity.json");
        let mut a = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap();
        assert_ne!(root.root.public_key, a.profile().public_key);
        a.save_new(&path).unwrap();
        let mut reloaded = DeviceIdentity::load(&path).unwrap();
        assert_eq!(a.profile(), reloaded.profile());
        assert_eq!(a.tls_private_key(), reloaded.tls_private_key());
        assert_eq!(
            root.root,
            reloaded.initialize_group(2000, 100000).unwrap().root
        );
        assert!(DeviceIdentity::generate().unwrap().save_new(&path).is_err());
        std::fs::write(&path, b"interrupted identity write").unwrap();
        assert!(DeviceIdentity::load(&path).is_err());
    }
}
