//! (v240) Clés d'accès du système : sous Windows, la fenêtre de sécurité de Windows (Windows
//! Hello : visage, empreinte ou code PIN ; clé de sécurité USB ou NFC ; téléphone), par
//! `webauthn.dll`, chargée à l'usage (le jeu démarre sans elle). Ailleurs : pas de clés
//! d'accès du système.
//!
//! Les appels attendent la réponse du joueur dans la fenêtre de Windows : à faire hors du fil
//! du rendu.

/// Clé créée : son identifiant et les données de l'authentificateur (avec la clé publique).
#[derive(Clone, Debug)]
pub struct Created {
    pub id: Vec<u8>,
    pub authenticator_data: Vec<u8>,
}

/// Connexion : la clé utilisée, ses données, la signature, l'identifiant d'utilisateur.
#[derive(Clone, Debug)]
pub struct Assertion {
    pub id: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Vec<u8>,
}

/// Les clés d'accès du système sont-elles là ?
pub fn available() -> bool {
    imp::available()
}

/// Crée une clé d'accès (découvrable, utilisateur vérifié) pour `rp_id`.
pub fn create(
    rp_id: &str,
    rp_name: &str,
    user_handle: &[u8],
    user_name: &str,
    client_data: &[u8],
    algorithms: &[i32],
) -> Result<Created, String> {
    imp::create(rp_id, rp_name, user_handle, user_name, client_data, algorithms)
}

/// Demande une clé d'accès de `rp_id` (n'importe laquelle : le système les propose).
pub fn get(rp_id: &str, client_data: &[u8]) -> Result<Assertion, String> {
    imp::get(rp_id, client_data)
}

#[cfg(not(windows))]
mod imp {
    use super::{Assertion, Created};

    pub fn available() -> bool {
        false
    }

    pub fn create(
        _rp_id: &str,
        _rp_name: &str,
        _user_handle: &[u8],
        _user_name: &str,
        _client_data: &[u8],
        _algorithms: &[i32],
    ) -> Result<Created, String> {
        Err("Clés d'accès : seulement sous Windows.".into())
    }

    pub fn get(_rp_id: &str, _client_data: &[u8]) -> Result<Assertion, String> {
        Err("Clés d'accès : seulement sous Windows.".into())
    }
}

#[cfg(windows)]
mod imp {
    use super::{Assertion, Created};
    use std::sync::OnceLock;
    use windows::core::{s, w, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{BOOL, HWND};
    use windows::Win32::Networking::WindowsWebServices::*;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    type MakeCredential = unsafe extern "system" fn(
        HWND,
        *const WEBAUTHN_RP_ENTITY_INFORMATION,
        *const WEBAUTHN_USER_ENTITY_INFORMATION,
        *const WEBAUTHN_COSE_CREDENTIAL_PARAMETERS,
        *const WEBAUTHN_CLIENT_DATA,
        *const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
        *mut *mut WEBAUTHN_CREDENTIAL_ATTESTATION,
    ) -> HRESULT;
    type GetAssertion = unsafe extern "system" fn(
        HWND,
        PCWSTR,
        *const WEBAUTHN_CLIENT_DATA,
        *const WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
        *mut *mut WEBAUTHN_ASSERTION,
    ) -> HRESULT;
    type FreeAttestation = unsafe extern "system" fn(*const WEBAUTHN_CREDENTIAL_ATTESTATION);
    type FreeAssertion = unsafe extern "system" fn(*const WEBAUTHN_ASSERTION);
    type ApiVersion = unsafe extern "system" fn() -> u32;
    type ErrorName = unsafe extern "system" fn(HRESULT) -> PCWSTR;

    struct Api {
        version: u32,
        make: MakeCredential,
        get: GetAssertion,
        free_attestation: FreeAttestation,
        free_assertion: FreeAssertion,
        error_name: ErrorName,
    }

    /// `webauthn.dll` (Windows 10 1903 et plus), chargée une fois.
    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let h = LoadLibraryW(w!("webauthn.dll")).ok()?;
            let make = GetProcAddress(h, s!("WebAuthNAuthenticatorMakeCredential"))?;
            let get = GetProcAddress(h, s!("WebAuthNAuthenticatorGetAssertion"))?;
            let fa = GetProcAddress(h, s!("WebAuthNFreeCredentialAttestation"))?;
            let fs = GetProcAddress(h, s!("WebAuthNFreeAssertion"))?;
            let ver = GetProcAddress(h, s!("WebAuthNGetApiVersionNumber"))?;
            let name = GetProcAddress(h, s!("WebAuthNGetErrorName"))?;
            let ver: ApiVersion = std::mem::transmute(ver);
            let version = ver();
            log::info!("clés d'accès : webauthn.dll, version {version} de l'API");
            Some(Api {
                version,
                make: std::mem::transmute::<_, MakeCredential>(make),
                get: std::mem::transmute::<_, GetAssertion>(get),
                free_attestation: std::mem::transmute::<_, FreeAttestation>(fa),
                free_assertion: std::mem::transmute::<_, FreeAssertion>(fs),
                error_name: std::mem::transmute::<_, ErrorName>(name),
            })
        })
        .as_ref()
    }

    pub fn available() -> bool {
        api().is_some_and(|a| a.version >= 1)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Message d'une erreur de Windows, en mots du jeu.
    fn error(api: &Api, hr: HRESULT) -> String {
        // NTE_USER_CANCELLED, NTE_NOT_FOUND, NTE_EXISTS
        match hr.0 as u32 {
            0x8009_0036 => "Annulé.".into(),
            0x8009_0011 => "Aucune clé d'accès de Minelib sur cet appareil.".into(),
            0x8009_000F => "Cette clé d'accès existe déjà.".into(),
            _ => {
                let name = unsafe { (api.error_name)(hr) };
                let name = if name.is_null() {
                    String::new()
                } else {
                    unsafe { name.to_string() }.unwrap_or_default()
                };
                format!("Clé d'accès refusée par Windows : {name} ({:08x}).", hr.0 as u32)
            }
        }
    }

    unsafe fn bytes(p: *const u8, n: u32) -> Vec<u8> {
        if p.is_null() || n == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p, n as usize).to_vec()
        }
    }

    pub fn create(
        rp_id: &str,
        rp_name: &str,
        user_handle: &[u8],
        user_name: &str,
        client_data: &[u8],
        algorithms: &[i32],
    ) -> Result<Created, String> {
        let api = api().ok_or("Clés d'accès : webauthn.dll absente (Windows 10 1903 ou plus).")?;
        let rp_id_w = wide(rp_id);
        let rp_name_w = wide(rp_name);
        let user_w = wide(user_name);
        let mut handle = user_handle.to_vec();
        let mut data = client_data.to_vec();
        let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
            pwszId: PCWSTR(rp_id_w.as_ptr()),
            pwszName: PCWSTR(rp_name_w.as_ptr()),
            pwszIcon: PCWSTR::null(),
        };
        let user = WEBAUTHN_USER_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
            cbId: handle.len() as u32,
            pbId: handle.as_mut_ptr(),
            pwszName: PCWSTR(user_w.as_ptr()),
            pwszIcon: PCWSTR::null(),
            pwszDisplayName: PCWSTR(user_w.as_ptr()),
        };
        let mut params: Vec<WEBAUTHN_COSE_CREDENTIAL_PARAMETER> = algorithms
            .iter()
            .map(|&alg| WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
                dwVersion: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
                pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
                lAlg: alg,
            })
            .collect();
        let cose = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS {
            cCredentialParameters: params.len() as u32,
            pCredentialParameters: params.as_mut_ptr(),
        };
        let cd = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: data.len() as u32,
            pbClientDataJSON: data.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        // Version 3 des options (Windows 10 1903) : le reste de la structure n'est pas lu.
        let opts = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_3,
            dwTimeoutMilliseconds: 120_000,
            dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            bRequireResidentKey: BOOL(1),
            dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
            dwAttestationConveyancePreference: WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
            ..Default::default()
        };
        let mut out: *mut WEBAUTHN_CREDENTIAL_ATTESTATION = std::ptr::null_mut();
        let hr = unsafe {
            (api.make)(
                GetForegroundWindow(),
                &rp,
                &user,
                &cose,
                &cd,
                &opts,
                &mut out,
            )
        };
        if hr.is_err() || out.is_null() {
            return Err(error(api, hr));
        }
        let r = unsafe {
            let a = &*out;
            let r = Created {
                id: bytes(a.pbCredentialId, a.cbCredentialId),
                authenticator_data: bytes(a.pbAuthenticatorData, a.cbAuthenticatorData),
            };
            (api.free_attestation)(out);
            r
        };
        Ok(r)
    }

    pub fn get(rp_id: &str, client_data: &[u8]) -> Result<Assertion, String> {
        let api = api().ok_or("Clés d'accès : webauthn.dll absente (Windows 10 1903 ou plus).")?;
        let rp_id_w = wide(rp_id);
        let mut data = client_data.to_vec();
        let cd = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: data.len() as u32,
            pbClientDataJSON: data.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        // Sans liste de clés : Windows propose celles de Minelib qu'il connaît.
        let opts = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4,
            dwTimeoutMilliseconds: 120_000,
            dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
            ..Default::default()
        };
        let mut out: *mut WEBAUTHN_ASSERTION = std::ptr::null_mut();
        let hr = unsafe {
            (api.get)(
                GetForegroundWindow(),
                PCWSTR(rp_id_w.as_ptr()),
                &cd,
                &opts,
                &mut out,
            )
        };
        if hr.is_err() || out.is_null() {
            return Err(error(api, hr));
        }
        let r = unsafe {
            let a = &*out;
            let r = Assertion {
                id: bytes(a.Credential.pbId, a.Credential.cbId),
                authenticator_data: bytes(a.pbAuthenticatorData, a.cbAuthenticatorData),
                signature: bytes(a.pbSignature, a.cbSignature),
                user_handle: bytes(a.pbUserId, a.cbUserId),
            };
            (api.free_assertion)(out);
            r
        };
        Ok(r)
    }
}
