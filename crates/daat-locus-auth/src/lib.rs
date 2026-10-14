//! Shared browser/native OPAQUE protocol. Passwords stay with the client.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, RegistrationRequest, RegistrationResponse, RegistrationUpload, ServerLogin,
    ServerLoginParameters, ServerRegistration, ServerSetup,
    argon2::{Algorithm, Argon2, Params, Version},
    ciphersuite::CipherSuite,
};
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::{Zeroize, Zeroizing};

pub struct Suite;
impl CipherSuite for Suite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = Argon2<'static>;
}
pub type Setup = ServerSetup<Suite>;
pub type PasswordFile = ServerRegistration<Suite>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticationFailed;
impl std::fmt::Display for AuthenticationFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Account authentication failed")
    }
}
impl std::error::Error for AuthenticationFailed {}
type Result<T> = std::result::Result<T, AuthenticationFailed>;
/// A one-login channel key. It never becomes a browser or device credential.
pub struct LoginChannel {
    secret: Zeroizing<Vec<u8>>,
    context: Vec<u8>,
}
impl LoginChannel {
    fn cipher(&self) -> Result<Aes256Gcm> {
        let mut key = Zeroizing::new([0u8; 32]);
        hkdf::Hkdf::<sha2::Sha256>::new(Some(b"DaatLocus.AccountEnrollment.v1"), &self.secret)
            .expand(&self.context, key.as_mut())
            .map_err(|_| AuthenticationFailed)?;
        Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| AuthenticationFailed)
    }
    pub fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.len() > 32 * 1024 {
            return Err(AuthenticationFailed);
        }
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher()?
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: bytes,
                    aad: &self.context,
                },
            )
            .map_err(|_| AuthenticationFailed)?;
        let mut result = nonce.to_vec();
        result.extend(ciphertext);
        Ok(result)
    }
    pub fn open(&self, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if bytes.len() < 28 || bytes.len() > 32 * 1024 + 28 {
            return Err(AuthenticationFailed);
        }
        let nonce: [u8; 12] = bytes[..12].try_into().map_err(|_| AuthenticationFailed)?;
        self.cipher()?
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &bytes[12..],
                    aad: &self.context,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| AuthenticationFailed)
    }
}
pub struct AuthenticatedLogin {
    pub message: Vec<u8>,
    pub channel: LoginChannel,
}
fn stretch() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19456, 2, 1, None).expect("fixed Argon2id parameters"),
    )
}
fn password(value: Vec<u8>) -> Result<Zeroizing<Vec<u8>>> {
    let value = Zeroizing::new(value);
    if value.is_empty() || value.len() > 1024 {
        return Err(AuthenticationFailed);
    }
    Ok(value)
}
pub fn new_setup() -> Setup {
    Setup::new(&mut OsRng)
}
pub fn registration_response(setup: &Setup, identifier: &[u8], request: &[u8]) -> Result<Vec<u8>> {
    let request = RegistrationRequest::deserialize(request).map_err(|_| AuthenticationFailed)?;
    Ok(ServerRegistration::start(setup, request, identifier)
        .map_err(|_| AuthenticationFailed)?
        .message
        .serialize()
        .to_vec())
}
pub fn password_file(upload: &[u8]) -> Result<PasswordFile> {
    Ok(ServerRegistration::finish(
        RegistrationUpload::deserialize(upload).map_err(|_| AuthenticationFailed)?,
    ))
}
pub struct RegistrationClient {
    state: ClientRegistration<Suite>,
    password: Zeroizing<Vec<u8>>,
    request: Vec<u8>,
}
impl RegistrationClient {
    pub fn start(value: Vec<u8>) -> Result<Self> {
        let password = password(value)?;
        let start =
            ClientRegistration::start(&mut OsRng, &password).map_err(|_| AuthenticationFailed)?;
        Ok(Self {
            state: start.state,
            password,
            request: start.message.serialize().to_vec(),
        })
    }
    pub fn request(&self) -> Vec<u8> {
        self.request.clone()
    }
    pub fn finish(self, response: &[u8]) -> Result<Vec<u8>> {
        let response =
            RegistrationResponse::deserialize(response).map_err(|_| AuthenticationFailed)?;
        let mut finish = self
            .state
            .finish(
                &mut OsRng,
                &self.password,
                response,
                ClientRegistrationFinishParameters {
                    ksf: Some(&stretch()),
                    ..Default::default()
                },
            )
            .map_err(|_| AuthenticationFailed)?;
        finish.export_key.zeroize();
        Ok(finish.message.serialize().to_vec())
    }
}
pub struct LoginClient {
    state: ClientLogin<Suite>,
    password: Zeroizing<Vec<u8>>,
    request: Vec<u8>,
}
impl LoginClient {
    pub fn start(value: Vec<u8>) -> Result<Self> {
        let password = password(value)?;
        let start = ClientLogin::start(&mut OsRng, &password).map_err(|_| AuthenticationFailed)?;
        Ok(Self {
            state: start.state,
            password,
            request: start.message.serialize().to_vec(),
        })
    }
    pub fn request(&self) -> Vec<u8> {
        self.request.clone()
    }
    pub fn finish(self, response: &[u8], context: &[u8]) -> Result<Vec<u8>> {
        Ok(self.authenticate(response, context)?.message)
    }
    pub fn authenticate(self, response: &[u8], context: &[u8]) -> Result<AuthenticatedLogin> {
        let response =
            CredentialResponse::deserialize(response).map_err(|_| AuthenticationFailed)?;
        let mut finish = self
            .state
            .finish(
                &mut OsRng,
                &self.password,
                response,
                ClientLoginFinishParameters {
                    context: Some(context),
                    ksf: Some(&stretch()),
                    ..Default::default()
                },
            )
            .map_err(|_| AuthenticationFailed)?;
        let secret = Zeroizing::new(finish.session_key.to_vec());
        finish.session_key.zeroize();
        finish.export_key.zeroize();
        Ok(AuthenticatedLogin {
            message: finish.message.serialize().to_vec(),
            channel: LoginChannel {
                secret,
                context: context.to_vec(),
            },
        })
    }
}
pub struct PendingLogin {
    state: ServerLogin<Suite>,
    context: Vec<u8>,
    response: Vec<u8>,
}
impl PendingLogin {
    pub fn start(
        setup: &Setup,
        record: Option<PasswordFile>,
        identifier: &[u8],
        request: &[u8],
        context: Vec<u8>,
    ) -> Result<Self> {
        let request = CredentialRequest::deserialize(request).map_err(|_| AuthenticationFailed)?;
        let start = ServerLogin::start(
            &mut OsRng,
            setup,
            record,
            request,
            identifier,
            ServerLoginParameters {
                context: Some(&context),
                ..Default::default()
            },
        )
        .map_err(|_| AuthenticationFailed)?;
        Ok(Self {
            state: start.state,
            context,
            response: start.message.serialize().to_vec(),
        })
    }
    pub fn response(&self) -> Vec<u8> {
        self.response.clone()
    }
    pub fn finish(self, finalization: &[u8]) -> Result<()> {
        self.authenticate(finalization).map(|_| ())
    }
    pub fn authenticate(self, finalization: &[u8]) -> Result<LoginChannel> {
        let finalization =
            CredentialFinalization::deserialize(finalization).map_err(|_| AuthenticationFailed)?;
        let mut finish = self
            .state
            .finish(
                finalization,
                ServerLoginParameters {
                    context: Some(&self.context),
                    ..Default::default()
                },
            )
            .map_err(|_| AuthenticationFailed)?;
        let secret = Zeroizing::new(finish.session_key.to_vec());
        finish.session_key.zeroize();
        Ok(LoginChannel {
            secret,
            context: self.context,
        })
    }
}

#[cfg(feature = "browser")]
mod browser {
    use super::*;
    use wasm_bindgen::prelude::*;
    fn failure(_: AuthenticationFailed) -> JsValue {
        JsValue::from_str("Account authentication failed")
    }
    #[wasm_bindgen]
    pub struct Registration {
        inner: Option<RegistrationClient>,
    }
    #[wasm_bindgen]
    impl Registration {
        #[wasm_bindgen(constructor)]
        pub fn new(password: Vec<u8>) -> std::result::Result<Registration, JsValue> {
            Ok(Self {
                inner: Some(RegistrationClient::start(password).map_err(failure)?),
            })
        }
        pub fn request(&self) -> std::result::Result<Vec<u8>, JsValue> {
            self.inner
                .as_ref()
                .map(RegistrationClient::request)
                .ok_or_else(|| failure(AuthenticationFailed))
        }
        pub fn finish(&mut self, response: Vec<u8>) -> std::result::Result<Vec<u8>, JsValue> {
            self.inner
                .take()
                .ok_or_else(|| failure(AuthenticationFailed))?
                .finish(&response)
                .map_err(failure)
        }
    }
    #[wasm_bindgen]
    pub struct Login {
        inner: Option<LoginClient>,
        channel: Option<LoginChannel>,
    }
    #[wasm_bindgen]
    impl Login {
        #[wasm_bindgen(constructor)]
        pub fn new(password: Vec<u8>) -> std::result::Result<Login, JsValue> {
            Ok(Self {
                inner: Some(LoginClient::start(password).map_err(failure)?),
                channel: None,
            })
        }
        pub fn request(&self) -> std::result::Result<Vec<u8>, JsValue> {
            self.inner
                .as_ref()
                .map(LoginClient::request)
                .ok_or_else(|| failure(AuthenticationFailed))
        }
        pub fn finish(
            &mut self,
            response: Vec<u8>,
            context: Vec<u8>,
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let authenticated = self
                .inner
                .take()
                .ok_or_else(|| failure(AuthenticationFailed))?
                .authenticate(&response, &context)
                .map_err(failure)?;
            self.channel = Some(authenticated.channel);
            Ok(authenticated.message)
        }
        pub fn open(&mut self, payload: Vec<u8>) -> std::result::Result<Vec<u8>, JsValue> {
            self.channel
                .take()
                .ok_or_else(|| failure(AuthenticationFailed))?
                .open(&payload)
                .map(|bytes| bytes.to_vec())
                .map_err(failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registration(setup: &Setup) -> PasswordFile {
        let client = RegistrationClient::start(b"isolated account password".to_vec()).unwrap();
        let response = registration_response(setup, b"account/name", &client.request()).unwrap();
        password_file(&client.finish(&response).unwrap()).unwrap()
    }
    #[test]
    fn account_enrollment_payload_requires_the_authenticated_login_key() {
        let setup = new_setup();
        let client = LoginClient::start(b"isolated account password".to_vec()).unwrap();
        let server = PendingLogin::start(
            &setup,
            Some(registration(&setup)),
            b"account/name",
            &client.request(),
            b"device/context".to_vec(),
        )
        .unwrap();
        let authenticated = client
            .authenticate(&server.response(), b"device/context")
            .unwrap();
        let channel = server.authenticate(&authenticated.message).unwrap();
        let encrypted = channel.seal(b"private enrollment record").unwrap();
        assert_eq!(
            &*authenticated.channel.open(&encrypted).unwrap(),
            b"private enrollment record"
        );
        let mut tampered = encrypted;
        tampered[15] ^= 1;
        assert!(authenticated.channel.open(&tampered).is_err());
        assert!(channel.open(b"invalid").is_err());
    }
    #[test]
    fn browser_native_protocol_authenticates_both_sides_and_binds_context() {
        let setup = new_setup();
        let record = registration(&setup);
        let client = LoginClient::start(b"isolated account password".to_vec()).unwrap();
        let server = PendingLogin::start(
            &setup,
            Some(record),
            b"account/name",
            &client.request(),
            b"account/stage/attempt".to_vec(),
        )
        .unwrap();
        let finalization = client
            .finish(&server.response(), b"account/stage/attempt")
            .unwrap();
        server.finish(&finalization).unwrap();
    }
    #[test]
    fn wrong_password_account_and_channel_cannot_authenticate() {
        let setup = new_setup();
        let record = registration(&setup).serialize();
        for (password, identifier, context) in [
            (
                b"wrong password".as_slice(),
                b"account/name".as_slice(),
                b"context".as_slice(),
            ),
            (
                b"isolated account password".as_slice(),
                b"another/name".as_slice(),
                b"context".as_slice(),
            ),
            (
                b"isolated account password".as_slice(),
                b"account/name".as_slice(),
                b"another context".as_slice(),
            ),
        ] {
            let client = LoginClient::start(password.to_vec()).unwrap();
            let server = PendingLogin::start(
                &setup,
                Some(PasswordFile::deserialize(&record).unwrap()),
                identifier,
                &client.request(),
                b"context".to_vec(),
            )
            .unwrap();
            assert!(client.finish(&server.response(), context).is_err());
        }
        let client = LoginClient::start(b"isolated account password".to_vec()).unwrap();
        let server = PendingLogin::start(
            &setup,
            None,
            b"account/name",
            &client.request(),
            b"context".to_vec(),
        )
        .unwrap();
        assert!(client.finish(&server.response(), b"context").is_err());
    }
}
