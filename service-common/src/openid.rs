use crate::error::ServiceError;
use anyhow::Context;
use envconfig::Envconfig;
use failure::Fail;
use failure::_core::fmt::Formatter;
use openid::Jws;
use reqwest::Certificate;
use std::fmt::Debug;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use url::Url;

const SERVICE_CA_CERT: &str = "/var/run/secrets/kubernetes.io/serviceaccount/service-ca.crt";

#[derive(Debug, Envconfig)]
pub struct AuthConfig {
    #[envconfig(from = "CLIENT_ID")]
    pub client_id: String,
    #[envconfig(from = "CLIENT_SECRET")]
    pub client_secret: String,
    #[envconfig(from = "ISSUER_URL")]
    pub issuer_url: String,
    #[envconfig(from = "REDIRECT_URL")]
    pub redirect_url: String,
    // Note: "roles" may be required for the "aud" claim when using Keycloak
    #[envconfig(from = "SCOPES", default = "openid profile email")]
    pub scopes: String,
}

pub struct Authenticator {
    pub client: Option<openid::Client>,
    pub scopes: String,
}

impl Debug for Authenticator {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct("Authenticator");

        match self.client {
            None => {
                d.field("client", &"None".to_string());
            }
            Some(_) => {
                d.field("client", &"Some(...)".to_string());
            }
        }

        d.finish()
    }
}

impl Authenticator {
    pub async fn validate_token(&self, token: String) -> Result<(), actix_web::Error> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| ServiceError::InternalError {
                message: "Missing an authenticator, when performing authentication".into(),
            })?;

        let mut token = Jws::new_encoded(&token);
        match client.decode_token(&mut token) {
            Ok(_) => Ok(()),
            Err(err) => {
                log::info!("Failed to decode token: {}", err);
                Err(ServiceError::AuthenticationError)
            }
        }?;

        log::info!("Token: {:#?}", token);

        match client.validate_token(&token, None, None) {
            Ok(_) => Ok(()),
            Err(err) => {
                log::info!("Validation failed: {}", err);
                Err(ServiceError::AuthenticationError.into())
            }
        }
    }
}

impl ClientConfig for AuthConfig {
    fn redirect_url(&self) -> Option<String> {
        Some(self.redirect_url.clone())
    }

    fn client_id(&self) -> String {
        self.client_id.clone()
    }

    fn client_secret(&self) -> String {
        self.client_secret.clone()
    }

    fn issuer_url(&self) -> String {
        self.issuer_url.clone()
    }
}

pub trait ClientConfig {
    fn redirect_url(&self) -> Option<String>;
    fn client_id(&self) -> String;
    fn client_secret(&self) -> String;
    fn issuer_url(&self) -> String;
}

pub async fn create_client(config: &dyn ClientConfig) -> anyhow::Result<openid::Client> {
    let mut client = reqwest::ClientBuilder::new();

    client = add_service_cert(client)?;

    let client = openid::DiscoveredClient::discover_with_client(
        client.build()?,
        config.client_id(),
        config.client_secret(),
        config.redirect_url(),
        Url::parse(&config.issuer_url())
            .with_context(|| format!("Failed to parse issuer URL: {}", config.issuer_url()))?,
    )
    .await
    .map_err(|err| anyhow::Error::from(err.compat()))?;

    log::info!("Discovered OpenID: {:#?}", client.config());

    Ok(client)
}

fn add_service_cert(mut client: reqwest::ClientBuilder) -> anyhow::Result<reqwest::ClientBuilder> {
    let cert = Path::new(SERVICE_CA_CERT);
    if cert.exists() {
        log::info!("Adding root certificate: {}", SERVICE_CA_CERT);
        let mut file = File::open(cert)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        let pems = pem::parse_many(buf);
        let pems = pems
            .into_iter()
            .map(|pem| {
                Certificate::from_pem(&pem::encode(&pem).into_bytes()).map_err(|err| err.into())
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        log::info!("Found {} certificates", pems.len());

        for pem in pems {
            log::info!("Adding root certificate: {:?}", pem);
            client = client.add_root_certificate(pem);
        }
    } else {
        log::info!(
            "Service CA certificate does not exist, skipping! ({})",
            SERVICE_CA_CERT
        );
    }

    Ok(client)
}