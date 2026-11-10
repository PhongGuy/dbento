//! The region and static credentials a DynamoDB connection signs its requests with.

use crate::secrets::Secret;
use crate::workspace::ConnectionConfig;

/// Static AWS credentials; `Debug` shows the key id and redacts the secret and token.
#[derive(Debug)]
pub(crate) struct AwsCredentials {
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: Secret,
    pub(crate) session_token: Option<Secret>,
}

/// `region`, else AWS_REGION, else AWS_DEFAULT_REGION; an empty variable is unset.
pub(crate) fn resolve_region(
    conn: &ConnectionConfig,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if let Some(region) = conn.region() {
        return Ok(region.to_owned());
    }
    var(&env, "AWS_REGION")
        .or_else(|| var(&env, "AWS_DEFAULT_REGION"))
        .ok_or_else(|| {
            "no region: set `region` in dbento.toml, or AWS_REGION or AWS_DEFAULT_REGION".to_owned()
        })
}

/// With `access_key_id`: that key and `secret`, no session token. Without: AWS_ACCESS_KEY_ID and
/// AWS_SECRET_ACCESS_KEY (both required) and AWS_SESSION_TOKEN when set.
pub(crate) fn resolve_credentials(
    conn: &ConnectionConfig,
    secret: Option<&Secret>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<AwsCredentials, String> {
    // A key written in the file pairs only with its stored secret; a session token from the
    // environment belongs to the environment's key, so it never rides along.
    if let Some(key) = conn.access_key_id() {
        let Some(secret) = secret else {
            return Err("no secret access key for `access_key_id`; \
                 store it with `dbento secret set <connection>`"
                .to_owned());
        };
        return Ok(AwsCredentials {
            access_key_id: key.to_owned(),
            secret_access_key: secret.clone(),
            session_token: None,
        });
    }
    let required = |name: &str| {
        var(&env, name).ok_or_else(|| {
            format!("{name} is not set; set `access_key_id` in dbento.toml or export the AWS_* variables")
        })
    };
    Ok(AwsCredentials {
        access_key_id: required("AWS_ACCESS_KEY_ID")?,
        secret_access_key: Secret::new(required("AWS_SECRET_ACCESS_KEY")?),
        session_token: var(&env, "AWS_SESSION_TOKEN").map(Secret::new),
    })
}

/// A variable's value, with an empty one read as unset.
fn var(env: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    env(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
