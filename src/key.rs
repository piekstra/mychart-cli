//! The per-install key a dynamic client signs with (docs/api.md, "Dynamic
//! client registration").
//!
//! Epic gives a public native app no refresh token. To keep access past the
//! first hour the app generates a key pair on the machine, registers the
//! public half as a dynamic client right after sign-in, and from then on
//! asks for access tokens with a JWT it signs itself (RFC 7523). The private
//! key lives only in the Keychain item for that (organization, patient).
//!
//! Epic accepts RSA keys in a static JWK Set (EC only from a JWK Set URL),
//! so the key is RSA, 3072 bits (Epic's minimum is 2048), signing RS384.

use base64::Engine as _;
use pk_cli_core::CliError;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::traits::PublicKeyParts;
use rsa::{Generate, PrivateKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha384};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The signing algorithm, as JWS names it.
pub const ALG: &str = "RS384";

/// How long a client assertion is good for. Epic refuses one whose `exp`
/// is more than five minutes after `iat`.
const ASSERTION_LIFETIME_SECS: i64 = 300;

/// An RSA key pair, kept as PKCS#8 DER.
pub struct SigningKey {
    key: PrivateKey,
}

impl SigningKey {
    pub fn generate() -> Result<Self, CliError> {
        let key = PrivateKey::try_generate()
            .map_err(|e| CliError::Other(format!("generating the per-install key: {e}")))?;
        Ok(SigningKey { key })
    }

    /// The PKCS#8 DER, base64 (standard alphabet), for the Keychain item.
    pub fn to_stored(&self) -> Result<String, CliError> {
        let der = self
            .key
            .to_pkcs8_der()
            .map_err(|e| CliError::Other(format!("encoding the per-install key: {e}")))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(der.as_bytes()))
    }

    pub fn from_stored(stored: &str) -> Result<Self, CliError> {
        let bad = |why: String| {
            CliError::Auth(format!("the stored per-install key cannot be read ({why})"))
                .with_remedy("sign in again with `mychart auth login`")
        };
        let der = base64::engine::general_purpose::STANDARD
            .decode(stored)
            .map_err(|e| bad(e.to_string()))?;
        let key = PrivateKey::from_pkcs8_der(&der).map_err(|e| bad(e.to_string()))?;
        Ok(SigningKey { key })
    }

    /// The RFC 7638 thumbprint of the public key, used as its `kid`.
    pub fn key_id(&self) -> String {
        let (n, e) = self.public_parts();
        let canonical = format!(r#"{{"e":"{e}","kty":"RSA","n":"{n}"}}"#);
        B64.encode(Sha256::digest(canonical.as_bytes()))
    }

    /// The public key as a JWK Set, for the registration request.
    pub fn public_jwks(&self) -> Value {
        let (n, e) = self.public_parts();
        json!({ "keys": [{
            "kty": "RSA",
            "n": n,
            "e": e,
            "alg": ALG,
            "use": "sig",
            "kid": self.key_id(),
        }] })
    }

    fn public_parts(&self) -> (String, String) {
        let public = self.key.to_public_key();
        let n = B64.encode(trim_leading_zeros(&public.n().to_be_bytes()));
        let e = B64.encode(trim_leading_zeros(&public.e().to_be_bytes()));
        (n, e)
    }

    /// A one-time client assertion (RFC 7523): `iss` and `sub` the dynamic
    /// client id, `aud` the token endpoint, valid five minutes from `now`.
    pub fn client_assertion(
        &self,
        client_id: &str,
        token_endpoint: &str,
        now: i64,
    ) -> Result<String, CliError> {
        let jti = crate::oauth::random_urlsafe(24)?;
        let header = json!({ "alg": ALG, "typ": "JWT", "kid": self.key_id() });
        let claims = json!({
            "iss": client_id,
            "sub": client_id,
            "aud": token_endpoint,
            "jti": jti,
            "iat": now,
            "nbf": now,
            "exp": now + ASSERTION_LIFETIME_SECS,
        });
        let signing_input = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(claims.to_string())
        );
        let signature = self
            .key
            .pkcs1v15_sign::<Sha384>(signing_input.as_bytes())
            .map_err(|e| CliError::Other(format!("signing the client assertion: {e}")))?;
        Ok(format!("{signing_input}.{}", B64.encode(signature)))
    }
}

/// JWK integers are unsigned big-endian with no leading zero bytes.
fn trim_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::Pkcs1v15Sign;

    #[test]
    fn a_key_signs_assertions_its_public_jwk_verifies() {
        let key = SigningKey::generate().unwrap();
        let restored = SigningKey::from_stored(&key.to_stored().unwrap()).unwrap();
        assert_eq!(restored.key_id(), key.key_id());

        let jwt = restored
            .client_assertion(
                "dyn-client-1",
                "https://auth.example.com/oauth2/token",
                1_700_000_000,
            )
            .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS384");
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "dyn-client-1");
        assert_eq!(claims["sub"], "dyn-client-1");
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            300
        );

        // The signature verifies with the key the JWK Set publishes.
        let jwk = &key.public_jwks()["keys"][0];
        let n =
            rsa::BoxedUint::from_be_slice_vartime(&B64.decode(jwk["n"].as_str().unwrap()).unwrap());
        let e =
            rsa::BoxedUint::from_be_slice_vartime(&B64.decode(jwk["e"].as_str().unwrap()).unwrap());
        let public = rsa::PublicKey::new(n, e).unwrap();
        let digest = Sha384::digest(format!("{}.{}", parts[0], parts[1]).as_bytes());
        public
            .verify(
                Pkcs1v15Sign::new::<Sha384>(),
                &digest,
                &B64.decode(parts[2]).unwrap(),
            )
            .unwrap();
        assert_eq!(jwk["e"], "AQAB");
    }

    #[test]
    fn a_damaged_stored_key_is_an_auth_error() {
        assert_eq!(
            SigningKey::from_stored("not base64!")
                .err()
                .unwrap()
                .exit_code(),
            3
        );
        assert_eq!(
            SigningKey::from_stored("AAAA").err().unwrap().exit_code(),
            3
        );
    }
}
