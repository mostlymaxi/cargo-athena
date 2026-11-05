//! S3 credentials for every CLI S3 call: the AWS SDK default chain (env,
//! `~/.aws` profiles incl. SSO / `credential_process` / `role_arn`, web
//! identity, ECS, IMDS), adapted into `object_store`.

use aws_config::Region;
use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::provider_config::ProviderConfig;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use object_store::aws::AwsCredential;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Refresh this long before expiry (object_store's own providers use 5 min).
const REFRESH: Duration = Duration::from_secs(300);

/// object_store asks once per request: resolve once, reuse until near expiry.
#[derive(Debug)]
pub(crate) struct S3Credentials {
    region: String,
    state: tokio::sync::Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    chain: Option<SharedCredentialsProvider>,
    cached: Option<(Arc<AwsCredential>, Option<SystemTime>)>,
}

impl S3Credentials {
    pub(crate) fn new(region: &str) -> Self {
        S3Credentials {
            region: region.to_string(),
            state: Default::default(),
        }
    }

    pub(crate) async fn resolve(&self) -> Result<Arc<AwsCredential>, String> {
        let mut st = self.state.lock().await;
        if let Some((c, exp)) = &st.cached
            && exp.is_none_or(|t| t > SystemTime::now() + REFRESH)
        {
            return Ok(c.clone());
        }
        // Built on first use: its HTTP client must live on the caller's runtime.
        if st.chain.is_none() {
            st.chain = Some(SharedCredentialsProvider::new(chain(&self.region).await));
        }
        let c = st.chain.as_ref().unwrap().provide_credentials().await;
        let c = c.map_err(|e| reasons(&e))?;
        let cred = Arc::new(AwsCredential {
            key_id: c.access_key_id().to_string(),
            secret_key: c.secret_access_key().to_string(),
            token: c.session_token().map(str::to_string),
        });
        st.cached = Some((cred.clone(), c.expiry()));
        Ok(cred)
    }
}

#[async_trait::async_trait]
impl object_store::CredentialProvider for S3Credentials {
    type Credential = AwsCredential;

    async fn get_credential(&self) -> object_store::Result<Arc<AwsCredential>> {
        self.resolve()
            .await
            .map_err(|m| object_store::Error::Generic {
                store: "S3",
                source: m.into(),
            })
    }
}

async fn chain(region: &str) -> DefaultCredentialsChain {
    DefaultCredentialsChain::builder()
        .region(Region::new(region.to_string()))
        .configure(provider_config())
        .build()
        .await
}

/// SDK providers panic without an HTTP client. This is the SDK default one
/// (env proxy included) with ring in place of aws-lc (C).
fn provider_config() -> ProviderConfig {
    use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
    use aws_smithy_http_client::{Builder, Connector, proxy::ProxyConfig};
    let https = Builder::new().build_with_connector_fn(|settings, rc| {
        let mut c = Connector::builder().tls_provider(Provider::Rustls(CryptoMode::Ring));
        c.set_connector_settings(settings.cloned());
        c.set_sleep_impl(rc.and_then(|rc| rc.sleep_impl()));
        c.set_proxy_config(Some(ProxyConfig::from_env()));
        c.build()
    });
    ProviderConfig::default().with_http_client(https)
}

/// The SDK's top-level message is generic; the reasons are in `source()`.
fn reasons(e: &dyn std::error::Error) -> String {
    let chain: Vec<String> = std::iter::successors(e.source(), |s| s.source())
        .map(|s| s.to_string())
        .collect();
    if chain.is_empty() {
        e.to_string()
    } else {
        chain.join(": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_credential_types::Credentials;
    use aws_credential_types::credential_fn::provide_credentials_fn;
    use aws_credential_types::provider::{self, error::CredentialsError};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// Resolves 3 times through `p`: the first result, and how often `p` ran.
    fn run(
        p: impl Fn() -> provider::Result + Send + Sync + 'static,
    ) -> (Result<Arc<AwsCredential>, String>, usize) {
        let n = Arc::new(AtomicUsize::new(0));
        let counter = n.clone();
        let chain = provide_credentials_fn(move || {
            counter.fetch_add(1, SeqCst);
            std::future::ready(p())
        });
        let mut creds = S3Credentials::new("");
        creds.state.get_mut().chain = Some(SharedCredentialsProvider::new(chain));
        let [first, ..] = crate::emulate::rt().block_on(async {
            [
                creds.resolve().await,
                creds.resolve().await,
                creds.resolve().await,
            ]
        });
        (first, n.load(SeqCst))
    }

    fn lookups(expiry: Option<SystemTime>) -> usize {
        let (c, n) = run(move || Ok(Credentials::new("k", "s", Some("t".into()), expiry, "t")));
        let c = c.unwrap();
        assert_eq!(
            (&*c.key_id, &*c.secret_key, c.token.as_deref()),
            ("k", "s", Some("t"))
        );
        n
    }

    #[test]
    fn caches_until_near_expiry() {
        let now = SystemTime::now();
        assert_eq!(lookups(None), 1);
        assert_eq!(lookups(Some(now + Duration::from_secs(3600))), 1);
        assert_eq!(lookups(Some(now + Duration::from_secs(60))), 3);
    }

    #[test]
    fn errors_show_the_reason_and_are_not_cached() {
        let (c, n) = run(|| Err(CredentialsError::not_loaded("no profile")));
        assert_eq!((c, n), (Err("no profile".to_string()), 3));
    }
}
