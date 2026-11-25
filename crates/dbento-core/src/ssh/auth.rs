//! Signing in to the SSH server once its host key is trusted.

use super::{ResolvedSsh, SshEnv};
use crate::secrets::SshSecrets;
use crate::session::ConnectError;
use crate::workspace::{ConnectionConfig, SshAuth};
use russh::MethodSet;
use russh::client::{AuthResult, Handle, Handler};

/// Signs in as `resolved.user` by the configured method; a failure names what to set or
/// which methods the server still accepts.
pub(crate) async fn authenticate<H: Handler>(
    handle: &mut Handle<H>,
    conn: &ConnectionConfig,
    resolved: &ResolvedSsh,
    _env: &SshEnv,
    secrets: &SshSecrets,
) -> Result<(), ConnectError> {
    if !matches!(resolved.auth, None | Some(SshAuth::Password)) {
        let method = resolved.auth.map_or("", SshAuth::name);
        return Err(ConnectError::SshAuth {
            message: format!("auth = \"{method}\" is not supported yet"),
        });
    }
    let login = resolved.login();
    let Some(password) = &secrets.password else {
        return Err(ConnectError::SshAuth {
            message: format!(
                "no SSH password is stored for {login}; set one with `dbento secret set {} --ssh-password`",
                conn.secret_owner()
            ),
        });
    };
    match handle
        .authenticate_password(&resolved.user, password.expose())
        .await
    {
        Ok(AuthResult::Success) => Ok(()),
        Ok(AuthResult::Failure {
            remaining_methods, ..
        }) => Err(ConnectError::SshAuth {
            message: format!(
                "the server refused the password for {login}; it accepts {}",
                methods(&remaining_methods)
            ),
        }),
        Err(e) => Err(ConnectError::SshAuth {
            message: format!("password login for {login} failed: {e}"),
        }),
    }
}

/// The server's remaining methods as a comma list, or a word when there are none.
fn methods(set: &MethodSet) -> String {
    if set.is_empty() {
        return "no other method".to_owned();
    }
    set.iter().map(<&str>::from).collect::<Vec<_>>().join(", ")
}
