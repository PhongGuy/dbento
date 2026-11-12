//! The connection form: one view that adds a connection or edits the keys of an existing one.
//!
//! The form only reads its controls back into a `ConnectionSpec` and emits events; the shell
//! owns saving, testing and the dialog around it.

use std::path::PathBuf;

use dbento_core::{
    AuthLevel, Color, ConnectionSpec, Engine, FieldKind, FolderPath, FormField, Secret,
    SecretSource, TlsMode, secret_env_var,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window, div, px,
};

use crate::confirm::{self, ChoiceStyle, with_keys};
use crate::keymap::{SaveConnection, chip_keys, keys_for};
use crate::sidebar::{color_name, tag_color};
use crate::theme;
use crate::ui::{self, Icons};

/// The colour tags offered, in the order the swatches show them.
pub const SWATCHES: [Color; 8] = [
    Color::Red,
    Color::Orange,
    Color::Yellow,
    Color::Green,
    Color::Teal,
    Color::Blue,
    Color::Purple,
    Color::Gray,
];

/// A text field of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The connection's name.
    Name,
    /// The SQLite file.
    Path,
    /// The Postgres host.
    Host,
    /// The Postgres port.
    Port,
    /// The MongoDB connection string.
    Uri,
    /// The SurrealDB namespace.
    Namespace,
    /// The Postgres database.
    Database,
    /// The Postgres user.
    User,
    /// The password, which goes only to the keychain.
    Password,
    /// The environment variable holding the secret.
    EnvVar,
    /// The extra root certificate file (`tls_ca`).
    TlsCa,
    /// The client certificate file (`tls_cert`).
    TlsCert,
    /// The client key file (`tls_key`).
    TlsKey,
    /// The client key's passphrase, which goes only to the keychain.
    KeyPassphrase,
    /// The SQL Server reader principal (`read_only_user`).
    ReadOnlyUser,
    /// The SurrealDB login's level (`auth_level`).
    AuthLevel,
    /// The DynamoDB region.
    Region,
    /// The DynamoDB endpoint override.
    Endpoint,
    /// The DynamoDB access key ID.
    AccessKeyId,
    /// The statement timeout.
    Timeout,
    /// The sidebar folder.
    Folder,
}

/// The example a field shows while empty; the password has none.
pub fn placeholder(field: Field) -> Option<&'static str> {
    match field {
        Field::Name => Some("e.g. shop-prod"),
        Field::Path => Some("path/to/database.sqlite"),
        Field::Host => Some("e.g. localhost"),
        Field::Port => Some("5432"),
        Field::Database => Some("e.g. postgres"),
        Field::User => Some("e.g. postgres"),
        Field::Password => None,
        Field::EnvVar => Some("e.g. SHOP_DB_PASSWORD"),
        Field::TlsCa => Some("e.g. certs/ca.pem"),
        Field::TlsCert => Some("e.g. certs/client.crt"),
        Field::TlsKey => Some("e.g. certs/client.key"),
        Field::KeyPassphrase => Some("only for an encrypted key"),
        Field::ReadOnlyUser => Some("e.g. dbento_reader"),
        Field::Uri => Some("e.g. mongodb://user@host:27017/database"),
        Field::Namespace => Some("e.g. app"),
        Field::AuthLevel => Some("root, namespace or database"),
        Field::Region => Some("e.g. eu-west-1"),
        Field::Endpoint => Some("optional"),
        Field::AccessKeyId => Some("optional"),
        Field::Timeout => Some("e.g. 30s"),
        Field::Folder => Some("e.g. Production"),
    }
}

/// Where the secret comes from, in the radio order of `conn-secret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretChoice {
    /// The OS keychain (also what an absent `secret` means).
    Keychain,
    /// An environment variable.
    Env,
    /// No secret.
    None,
}

/// The TLS mode, in the radio order of `conn-tls`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsChoice {
    /// No `tls` key: the engine's default.
    Default,
    /// `tls = "verify"`.
    Verify,
    /// `tls = "off"`.
    Off,
}

/// What the form asks of its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFormEvent {
    /// The user pressed Save.
    Save,
    /// The user pressed Cancel.
    Cancel,
    /// The user asked to reload a `dbento.toml` changed since load.
    Reload,
    /// The user pressed Test.
    Test,
    /// The user pressed Delete on an existing connection.
    Delete,
    /// A field changed, so a running Test no longer answers for the form.
    Edited,
}

/// Where Test connection stands, shown in `conn-test-result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestState {
    /// Not tested since the form opened.
    Idle,
    /// A test is connecting.
    Testing,
    /// The last test opened a session.
    Connected,
    /// The last test failed with this message.
    Failed(String),
}

/// The connection form view.
pub struct ConnectionForm {
    previous: Option<String>,
    loaded: ConnectionSpec,
    engine: Engine,
    name: Entity<InputState>,
    path: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    uri: Entity<InputState>,
    namespace: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    env_var: Entity<InputState>,
    tls_ca: Entity<InputState>,
    tls_cert: Entity<InputState>,
    tls_key: Entity<InputState>,
    key_passphrase: Entity<InputState>,
    read_only_user: Entity<InputState>,
    auth_level: Entity<InputState>,
    region: Entity<InputState>,
    endpoint: Entity<InputState>,
    access_key_id: Entity<InputState>,
    timeout: Entity<InputState>,
    folder: Entity<InputState>,
    secret: SecretChoice,
    tls: TlsChoice,
    read_only: bool,
    consistent_read: bool,
    color: Option<Color>,
    error: Option<String>,
    stale: bool,
    folders: Vec<FolderPath>,
    suggestions: Vec<FolderPath>,
    // The server's databases from the last Test that connected, kept until a field that
    // shapes the server or the login changes, and those offered under Database right now.
    databases: Vec<String>,
    database_suggestions: Vec<String>,
    base_dir: Option<PathBuf>,
    secrets_in_memory: bool,
    env_fallback: bool,
    test: TestState,
    // Held by the form's root so Ctrl+S reaches SaveConnection while no field has focus.
    focus: FocusHandle,
    _folder_changes: Subscription,
    _field_changes: Vec<Subscription>,
}

/// The most folder suggestions offered, and the database rows shown before the list scrolls.
const MAX_SUGGESTIONS: usize = 8;

/// One suggestion row's height.
const SUGGESTION_HEIGHT: f32 = 22.;

impl EventEmitter<ConnectionFormEvent> for ConnectionForm {}

impl Focusable for ConnectionForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConnectionForm {
    /// A form for a new connection (`existing` None, engine Postgres) or over `existing`'s keys as written.
    pub fn new(
        existing: Option<(String, ConnectionSpec)>,
        folders: Vec<FolderPath>,
        secrets_in_memory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (previous, loaded) = match existing {
            Some((name, spec)) => (Some(name), spec),
            None => (None, ConnectionSpec::new(Engine::Postgres)),
        };
        let shown = Shown::of(&loaded);
        let mut text = |field: Field, value: &str| {
            let (value, hint) = (value.to_owned(), placeholder(field).unwrap_or(""));
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(hint)
            })
        };
        let name = text(Field::Name, previous.as_deref().unwrap_or(""));
        let path = text(Field::Path, &shown.path);
        let host = text(Field::Host, &shown.host);
        let port = text(Field::Port, &shown.port);
        let uri = text(Field::Uri, &shown.uri);
        let namespace = text(Field::Namespace, &shown.namespace);
        let database = text(Field::Database, &shown.database);
        let user = text(Field::User, &shown.user);
        let env_var = text(Field::EnvVar, &shown.env_var);
        let tls_ca = text(Field::TlsCa, &shown.tls_ca);
        let tls_cert = text(Field::TlsCert, &shown.tls_cert);
        let tls_key = text(Field::TlsKey, &shown.tls_key);
        let read_only_user = text(Field::ReadOnlyUser, &shown.read_only_user);
        let auth_level = text(Field::AuthLevel, &shown.auth_level);
        let region = text(Field::Region, &shown.region);
        let endpoint = text(Field::Endpoint, &shown.endpoint);
        let access_key_id = text(Field::AccessKeyId, &shown.access_key_id);
        let timeout = text(Field::Timeout, &shown.timeout);
        let folder = text(Field::Folder, &shown.folder);
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let key_passphrase = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(placeholder(Field::KeyPassphrase).unwrap_or(""))
        });
        let folder_changes = cx.subscribe_in(&folder, window, |this, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.suggest(cx);
            }
        });
        // Typing in any field that shapes the session makes the last Test answer stale; one
        // that shapes the server or the login also drops the databases that Test listed.
        let server_inputs = [
            &path,
            &host,
            &port,
            &uri,
            &namespace,
            &user,
            &password,
            &env_var,
            &tls_ca,
            &tls_cert,
            &tls_key,
            &key_passphrase,
            &region,
            &endpoint,
            &access_key_id,
        ];
        let session_inputs = [&name, &read_only_user, &auth_level, &timeout];
        let mut field_changes: Vec<Subscription> = server_inputs
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, e: &InputEvent, _, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.touched(cx);
                    }
                })
            })
            .collect();
        field_changes.extend(session_inputs.into_iter().map(|input| {
            cx.subscribe_in(input, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.untested(cx);
                }
            })
        }));
        // The database typed so far narrows the listed ones; the list itself stays.
        field_changes.push(
            cx.subscribe_in(&database, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.untested(cx);
                    this.suggest_databases(cx);
                }
            }),
        );
        let form = ConnectionForm {
            previous,
            engine: loaded.engine,
            name,
            path,
            host,
            port,
            uri,
            namespace,
            database,
            user,
            password,
            env_var,
            tls_ca,
            tls_cert,
            tls_key,
            key_passphrase,
            read_only_user,
            auth_level,
            region,
            endpoint,
            access_key_id,
            timeout,
            folder,
            secret: shown.secret,
            tls: shown.tls,
            read_only: shown.read_only,
            consistent_read: shown.consistent_read,
            color: loaded.color,
            loaded,
            error: None,
            stale: false,
            folders,
            suggestions: Vec::new(),
            databases: Vec::new(),
            database_suggestions: Vec::new(),
            base_dir: None,
            secrets_in_memory,
            env_fallback: false,
            test: TestState::Idle,
            focus: cx.focus_handle(),
            _folder_changes: folder_changes,
            _field_changes: field_changes,
        };
        form.show_placeholders(window, cx);
        form
    }

    // The engine's descriptor names each of its fields' examples; fields it lacks keep theirs.
    fn show_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        for f in descriptor_fields(self.engine) {
            if let Some((field, _)) = field_of(f.key) {
                self.input(field)
                    .clone()
                    .update(cx, |s, cx| s.set_placeholder(f.placeholder, window, cx));
            }
        }
    }

    /// A new-connection form prefilled from `spec` and named `name`: `previous` is None and the
    /// password empty, so Save adds a connection and no secret is copied.
    pub fn copy_of(
        name: &str,
        spec: ConnectionSpec,
        folders: Vec<FolderPath>,
        secrets_in_memory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut form = Self::new(
            Some((name.to_owned(), spec)),
            folders,
            secrets_in_memory,
            window,
            cx,
        );
        form.previous = None;
        form
    }

    /// Shows a picked file under `dir` relative to it, as `dbento.toml` resolves paths.
    pub fn with_base_dir(mut self, dir: PathBuf) -> Self {
        self.base_dir = Some(dir);
        self
    }

    /// Marks the old name's `DBENTO_SECRET_<NAME>` as set, so a rename notes the new variable.
    pub fn with_env_fallback(mut self, set: bool) -> Self {
        self.env_fallback = set;
        self
    }

    /// Moves the form onto `loaded`, the edited connection's keys after a reload: a control
    /// the user left as first shown takes the new value, a changed one keeps the user's input.
    /// `None` keeps the form as it is and leaves Save to report the missing connection.
    pub fn rebase(
        &mut self,
        loaded: Option<ConnectionSpec>,
        folders: Vec<FolderPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.folders = folders;
        self.suggestions.clear();
        // A reload may move the server under the form, so a Test lists the databases again.
        self.forget_databases();
        let Some(loaded) = loaded else {
            cx.notify();
            return;
        };
        let (was, now) = (Shown::of(&self.loaded), Shown::of(&loaded));
        for (field, was, now) in [
            (Field::Path, was.path, now.path),
            (Field::Host, was.host, now.host),
            (Field::Port, was.port, now.port),
            (Field::Uri, was.uri, now.uri),
            (Field::Namespace, was.namespace, now.namespace),
            (Field::Database, was.database, now.database),
            (Field::User, was.user, now.user),
            (Field::EnvVar, was.env_var, now.env_var),
            (Field::TlsCa, was.tls_ca, now.tls_ca),
            (Field::TlsCert, was.tls_cert, now.tls_cert),
            (Field::TlsKey, was.tls_key, now.tls_key),
            (Field::ReadOnlyUser, was.read_only_user, now.read_only_user),
            (Field::AuthLevel, was.auth_level, now.auth_level),
            (Field::Region, was.region, now.region),
            (Field::Endpoint, was.endpoint, now.endpoint),
            (Field::AccessKeyId, was.access_key_id, now.access_key_id),
            (Field::Timeout, was.timeout, now.timeout),
            (Field::Folder, was.folder, now.folder),
        ] {
            let input = self.input(field).clone();
            if input.read(cx).value().trim() == was.trim() {
                input.update(cx, |s, cx| s.set_value(now, window, cx));
            }
        }
        if self.engine == self.loaded.engine {
            self.engine = loaded.engine;
            self.show_placeholders(window, cx);
        }
        if self.secret == was.secret {
            self.secret = now.secret;
        }
        if self.tls == was.tls {
            self.tls = now.tls;
        }
        if self.read_only == was.read_only {
            self.read_only = now.read_only;
        }
        if self.consistent_read == was.consistent_read {
            self.consistent_read = now.consistent_read;
        }
        if self.color == self.loaded.color {
            self.color = loaded.color;
        }
        self.loaded = loaded;
        cx.notify();
    }

    /// The folder paths offered under `conn-folder` for the text typed so far.
    pub fn suggestions(&self) -> &[FolderPath] {
        &self.suggestions
    }

    // Offers the existing folders that extend the typed text; nothing typed offers nothing,
    // so the list only appears while the user is choosing a folder.
    fn suggest(&mut self, cx: &mut Context<Self>) {
        let typed = self.folder.read(cx).value().trim().to_owned();
        self.suggestions = if typed.is_empty() {
            Vec::new()
        } else {
            self.folders
                .iter()
                .filter(|f| f.as_str().starts_with(&typed) && f.as_str() != typed)
                .take(MAX_SUGGESTIONS)
                .cloned()
                .collect()
        };
        cx.notify();
    }

    // Asks for one file for `field`; a file inside the workspace folder is shown relative
    // to it, as `dbento.toml` resolves paths, and one outside stays absolute.
    fn pick_file(
        &mut self,
        field: Field,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reply = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        let base = self.base_dir.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = reply.await else {
                return;
            };
            let Some(file) = paths.into_iter().next() else {
                return;
            };
            let shown = base
                .and_then(|b| file.strip_prefix(b).ok().map(PathBuf::from))
                .unwrap_or(file);
            let _ = this.update_in(cx, |form, window, cx| {
                form.input(field).clone().update(cx, |s, cx| {
                    s.set_value(shown.to_string_lossy().into_owned(), window, cx)
                });
                // set_value emits no Change, so a picked file resets the answer here.
                form.touched(cx);
            });
        })
        .detach();
    }

    /// The name the edited connection had when the form opened; None for a new one.
    pub fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }

    /// The input behind a text field.
    pub fn input(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Name => &self.name,
            Field::Path => &self.path,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Uri => &self.uri,
            Field::Namespace => &self.namespace,
            Field::Database => &self.database,
            Field::User => &self.user,
            Field::Password => &self.password,
            Field::EnvVar => &self.env_var,
            Field::TlsCa => &self.tls_ca,
            Field::TlsCert => &self.tls_cert,
            Field::TlsKey => &self.tls_key,
            Field::KeyPassphrase => &self.key_passphrase,
            Field::ReadOnlyUser => &self.read_only_user,
            Field::AuthLevel => &self.auth_level,
            Field::Region => &self.region,
            Field::Endpoint => &self.endpoint,
            Field::AccessKeyId => &self.access_key_id,
            Field::Timeout => &self.timeout,
            Field::Folder => &self.folder,
        }
    }

    /// The typed password, when non-empty. Never prefilled.
    pub fn password(&self, cx: &App) -> Option<Secret> {
        let typed = self.password.read(cx).value();
        (!typed.is_empty()).then(|| Secret::new(typed.to_string()))
    }

    /// The typed client key passphrase, when non-empty and the engine takes a client key.
    /// Never prefilled.
    pub fn key_passphrase(&self, cx: &App) -> Option<Secret> {
        if !client_cert(self.engine) {
            return None;
        }
        let typed = self.key_passphrase.read(cx).value();
        (!typed.is_empty()).then(|| Secret::new(typed.to_string()))
    }

    /// The databases offered under `conn-database` for the text typed so far.
    pub fn database_suggestions(&self) -> &[String] {
        &self.database_suggestions
    }

    /// Takes the databases a Test listed on the server and offers all but the one Database
    /// names: Postgres needs a database to test with, so the field is rarely empty here.
    /// An empty list offers none.
    pub fn set_databases(&mut self, databases: Vec<String>, cx: &mut Context<Self>) {
        self.databases = databases;
        let named = self.database.read(cx).value().trim().to_owned();
        self.database_suggestions = self
            .databases
            .iter()
            .filter(|d| **d != named)
            .cloned()
            .collect();
        cx.notify();
    }

    // While the user types in Database, offers the listed databases that extend the text;
    // a name typed in full offers nothing more for it.
    fn suggest_databases(&mut self, cx: &mut Context<Self>) {
        let typed = self.database.read(cx).value().trim().to_owned();
        self.database_suggestions = self
            .databases
            .iter()
            .filter(|d| d.starts_with(&typed) && **d != typed)
            .cloned()
            .collect();
        cx.notify();
    }

    // A picked database closes the list and, like typing it, drops the last Test answer: that
    // answer was for the database the form named then.
    fn database_picked(&mut self, cx: &mut Context<Self>) {
        self.database_suggestions.clear();
        self.untested(cx);
    }

    fn forget_databases(&mut self) {
        self.databases.clear();
        self.database_suggestions.clear();
    }

    // A change to what reaches the server or signs in: the listed databases may not be this
    // server's, so they go with the last Test answer.
    fn touched(&mut self, cx: &mut Context<Self>) {
        self.forget_databases();
        self.untested(cx);
    }

    // A change drops the last Test answer and any inline error, except the stale-file one,
    // which only Reload resolves.
    fn untested(&mut self, cx: &mut Context<Self>) {
        self.test = TestState::Idle;
        if !self.stale {
            self.error = None;
        }
        cx.emit(ConnectionFormEvent::Edited);
        cx.notify();
    }

    /// Shows a Test connection state under the fields.
    pub fn set_test(&mut self, state: TestState, cx: &mut Context<Self>) {
        self.test = state;
        cx.notify();
    }

    /// Where Test connection stands.
    pub fn test_state(&self) -> &TestState {
        &self.test
    }

    /// The engine currently chosen.
    pub fn engine(&self) -> Engine {
        self.engine
    }

    /// The name and the loaded spec with only the controls the user changed applied.
    pub fn edit(&self, cx: &App) -> Result<(String, ConnectionSpec), String> {
        let text = |field: Field| self.input(field).read(cx).value().trim().to_owned();
        let name = text(Field::Name);
        if name.is_empty() {
            return Err("Enter a name".into());
        }
        // Compare each control with what the loaded spec showed, so an untouched control never
        // writes a key: a default stays absent and a value keeps its spelling in the file.
        let mut spec = self.loaded.clone();
        if spec.engine != self.engine {
            spec.engine = self.engine;
            clear_other_engine(&mut spec);
        }
        let shown = Shown::of(&spec);
        // Inputs are read trimmed, so the loaded value is too: untouched padding is no change.
        let changed = |field: Field, was: &str| {
            let now = text(field);
            (now != was.trim()).then(|| (!now.is_empty()).then_some(now))
        };
        for f in descriptor_fields(self.engine) {
            match f.kind {
                FieldKind::Port { .. } => {
                    if let Some(v) = changed(Field::Port, &shown.port) {
                        spec.port = v.map(|p| parse_port(&p)).transpose()?;
                    }
                }
                FieldKind::Secret => {
                    let env = text(Field::EnvVar);
                    if self.secret != shown.secret
                        || (self.secret == SecretChoice::Env && env != shown.env_var.trim())
                    {
                        spec.secret = Some(match self.secret {
                            SecretChoice::Keychain => SecretSource::Keychain,
                            SecretChoice::None => SecretSource::None,
                            SecretChoice::Env if env.is_empty() => {
                                return Err("Enter an environment variable name".into());
                            }
                            SecretChoice::Env => SecretSource::Env(env),
                        });
                    }
                }
                FieldKind::Tls => {
                    let pairs = if client_cert(self.engine) { 3 } else { 1 };
                    for (field, was, key) in [
                        (Field::TlsCa, &shown.tls_ca, &mut spec.tls_ca),
                        (Field::TlsCert, &shown.tls_cert, &mut spec.tls_cert),
                        (Field::TlsKey, &shown.tls_key, &mut spec.tls_key),
                    ]
                    .into_iter()
                    .take(pairs)
                    {
                        if let Some(v) = changed(field, was) {
                            *key = v;
                        }
                    }
                    if self.tls != shown.tls {
                        spec.tls = match self.tls {
                            TlsChoice::Default => None,
                            TlsChoice::Verify => Some(TlsMode::Verify),
                            TlsChoice::Off => Some(TlsMode::Off),
                        };
                    }
                }
                // Only `consistent_read` is a flag today; ticked writes true, unticked drops the key.
                FieldKind::Flag => {
                    if f.key == "consistent_read" && self.consistent_read != shown.consistent_read {
                        spec.consistent_read = self.consistent_read.then_some(true);
                    }
                }
                FieldKind::Text | FieldKind::Path | FieldKind::Uri | _ => {
                    let Some((field, _)) = field_of(f.key) else {
                        continue;
                    };
                    if field == Field::AuthLevel {
                        if let Some(v) = changed(field, &shown.auth_level) {
                            spec.auth_level = v.map(|t| parse_auth_level(&t)).transpose()?;
                        }
                        continue;
                    }
                    let (was, key) = match field {
                        Field::Path => (&shown.path, &mut spec.path),
                        Field::Host => (&shown.host, &mut spec.host),
                        Field::Uri => (&shown.uri, &mut spec.uri),
                        Field::Namespace => (&shown.namespace, &mut spec.namespace),
                        Field::Database => (&shown.database, &mut spec.database),
                        Field::User => (&shown.user, &mut spec.user),
                        Field::ReadOnlyUser => (&shown.read_only_user, &mut spec.read_only_user),
                        Field::Region => (&shown.region, &mut spec.region),
                        Field::Endpoint => (&shown.endpoint, &mut spec.endpoint),
                        Field::AccessKeyId => (&shown.access_key_id, &mut spec.access_key_id),
                        Field::Name
                        | Field::Port
                        | Field::Password
                        | Field::EnvVar
                        | Field::TlsCa
                        | Field::TlsCert
                        | Field::TlsKey
                        | Field::KeyPassphrase
                        | Field::AuthLevel
                        | Field::Timeout
                        | Field::Folder => continue,
                    };
                    if let Some(v) = changed(field, was) {
                        *key = v;
                    }
                }
            }
        }
        if self.read_only != shown.read_only {
            spec.read_only = Some(self.read_only);
        }
        if let Some(v) = changed(Field::Timeout, &shown.timeout) {
            spec.statement_timeout = v;
        }
        if self.color != spec.color {
            spec.color = self.color;
        }
        if let Some(v) = changed(Field::Folder, &shown.folder) {
            spec.folder = v
                .map(|f| f.parse::<FolderPath>())
                .transpose()
                .map_err(|e| e.to_string())?;
        }
        Ok((name, spec))
    }

    /// Shows `error` under the fields; `stale` adds the Reload button for a file changed since load.
    pub fn set_error(&mut self, error: Option<String>, stale: bool, cx: &mut Context<Self>) {
        self.error = error;
        self.stale = stale;
        cx.notify();
    }

    /// The error line's text, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Switching engine empties the fields only the other engine has, so the result validates;
    /// switching back to the loaded engine shows its loaded values again.
    fn set_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if engine == self.engine {
            return;
        }
        let old = std::mem::replace(&mut self.engine, engine);
        self.show_placeholders(window, cx);
        let (was, now) = (descriptor_fields(old), descriptor_fields(engine));
        let port_default = |fields: &[FormField]| {
            fields.iter().find_map(|f| match f.kind {
                FieldKind::Port { default } => Some(default.to_string()),
                FieldKind::Text
                | FieldKind::Path
                | FieldKind::Secret
                | FieldKind::Tls
                | FieldKind::Uri
                | FieldKind::Flag
                | _ => None,
            })
        };
        if engine == self.loaded.engine {
            // Back on the loaded engine, only what the engine left behind had cleared returns;
            // a key both engines show keeps what was typed while away.
            let shown = Shown::of(&self.loaded);
            let had = |key: &str| was.iter().any(|f| f.key == key);
            let mut back: Vec<Field> = now
                .iter()
                .filter(|f| !had(f.key))
                .flat_map(fields_owned)
                .collect();
            if self.port.read(cx).value().trim().is_empty()
                && port_default(&now).is_some_and(|p| shown.port.trim() == p)
            {
                back.push(Field::Port);
            }
            for field in back {
                let value = shown.value(field);
                self.input(field)
                    .clone()
                    .update(cx, |s, cx| s.set_value(value, window, cx));
            }
            if !was.iter().any(|f| f.kind == FieldKind::Secret) {
                self.secret = shown.secret;
            }
            if !was.iter().any(|f| f.kind == FieldKind::Tls) {
                self.tls = shown.tls;
            }
            if !had("consistent_read") {
                self.consistent_read = shown.consistent_read;
            }
            cx.notify();
            return;
        }
        // Keys both engines show keep their text; a port left at the old default would
        // otherwise pin the new engine to the wrong one, so it clears too.
        let has = |key: &str| now.iter().any(|f| f.key == key);
        let mut leaving: Vec<Field> = was
            .iter()
            .filter(|f| !has(f.key))
            .flat_map(fields_owned)
            .collect();
        if port_default(&was).is_some_and(|p| self.port.read(cx).value().trim() == p) {
            leaving.push(Field::Port);
        }
        for field in leaving {
            self.input(field)
                .clone()
                .update(cx, |s, cx| s.set_value("", window, cx));
        }
        if !now.iter().any(|f| f.kind == FieldKind::Secret) {
            self.secret = SecretChoice::Keychain;
        }
        if !now.iter().any(|f| f.kind == FieldKind::Tls) {
            self.tls = TlsChoice::Default;
        }
        if !has("consistent_read") {
            self.consistent_read = false;
        }
        cx.notify();
    }

    fn row(label: &'static str, control: impl IntoElement, cx: &App) -> AnyElement {
        h_flex()
            .gap_3()
            .min_h(px(32.))
            .child(
                div()
                    .w(px(LABEL_WIDTH))
                    .flex_none()
                    .text_sm()
                    .text_color(theme::dim(cx))
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
            .into_any_element()
    }

    fn text_row(
        &self,
        label: &'static str,
        id: &'static str,
        field: Field,
        cx: &App,
    ) -> AnyElement {
        let input = Self::styled(Input::new(self.input(field)), field != Field::Name, cx);
        let input = if matches!(field, Field::Password | Field::KeyPassphrase) {
            input.mask_toggle()
        } else {
            input
        };
        Self::row(label, div().id(id).test_support().child(input), cx)
    }

    // A path field with a picker button after it, the button's id being the field's plus `-pick`.
    fn file_row(
        &self,
        label: &'static str,
        id: &'static str,
        field: Field,
        prompt: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pick = Button::new(SharedString::from(format!("{id}-pick")))
            .small()
            .ghost()
            .icon(Icons::FolderOpen)
            .tooltip("Choose file")
            .on_click(
                cx.listener(move |this, _, window, cx| this.pick_file(field, prompt, window, cx)),
            );
        let file = h_flex()
            .gap_1()
            .child(
                div()
                    .id(id)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .child(Self::styled(Input::new(self.input(field)), true, cx)),
            )
            .child(pick);
        Self::row(label, file, cx)
    }

    // A field and Port share one row: the field fills it and the port, at most five digits,
    // keeps a fixed 96 px box after its own label.
    fn host_port_row(
        &self,
        label: &'static str,
        id: &'static str,
        field: Field,
        cx: &App,
    ) -> AnyElement {
        let host = Self::styled(Input::new(self.input(field)), true, cx);
        let port = Self::styled(Input::new(&self.port), true, cx).w(px(PORT_WIDTH));
        let line = h_flex()
            .gap_3()
            .child(div().id(id).test_support().flex_1().min_w_0().child(host))
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(theme::dim(cx))
                    .child("Port"),
            )
            .child(
                div()
                    .id("conn-port")
                    .test_support()
                    .flex_none()
                    .w(px(PORT_WIDTH))
                    .child(port),
            );
        Self::row(label, line, cx)
    }

    // A dim line under a row, aligned with the controls, as the notes are.
    fn help_line(id: SharedString, says: &'static str, cx: &App) -> AnyElement {
        div()
            .id(id)
            .aria_label(says)
            .test_support()
            .pl(px(LABEL_WIDTH + 12.))
            .text_xs()
            .text_color(theme::dim(cx))
            .child(says)
            .into_any_element()
    }

    // The Password row and the rows that say where the secret comes from.
    fn secret_rows(
        &self,
        label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = vec![self.text_row(label, "conn-password", Field::Password, cx)];
        let secret = ui::segmented(
            "conn-secret",
            &["Keychain", "Environment variable", "None"],
            self.secret as usize,
            cx.listener(|this, ix: &usize, _, cx| {
                this.secret = [
                    SecretChoice::Keychain,
                    SecretChoice::Env,
                    SecretChoice::None,
                ][*ix];
                this.touched(cx);
            }),
            window,
            cx,
        );
        rows.push(Self::row("Secret", h_flex().child(secret), cx));
        if self.secret == SecretChoice::Env {
            rows.push(self.text_row("Variable", "conn-env", Field::EnvVar, cx));
        }
        if self.secrets_in_memory && self.secret == SecretChoice::Keychain {
            rows.push(self.keychain_note(cx));
        }
        if self.secret == SecretChoice::Keychain {
            rows.extend(self.env_note(cx));
        }
        rows
    }

    // The TLS mode and, unless TLS is off, the certificate rows.
    fn tls_rows(
        &self,
        label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tls = ui::segmented(
            "conn-tls",
            &["Default", "Verify", "Off"],
            self.tls as usize,
            cx.listener(|this, ix: &usize, _, cx| {
                this.tls = [TlsChoice::Default, TlsChoice::Verify, TlsChoice::Off][*ix];
                this.touched(cx);
                cx.notify();
            }),
            window,
            cx,
        );
        let mut rows = vec![Self::row(label, h_flex().child(tls), cx)];
        // A client certificate only travels inside TLS, so its rows go with TLS off, and an
        // engine that takes no client certificate shows only the CA row.
        if self.tls != TlsChoice::Off {
            let files = if client_cert(self.engine) { 3 } else { 1 };
            for (label, id, field, prompt) in [
                (
                    "CA cert",
                    "conn-tls-ca",
                    Field::TlsCa,
                    "Choose CA certificate",
                ),
                (
                    "Client cert",
                    "conn-tls-cert",
                    Field::TlsCert,
                    "Choose client certificate",
                ),
                (
                    "Client key",
                    "conn-tls-key",
                    Field::TlsKey,
                    "Choose client key",
                ),
            ]
            .into_iter()
            .take(files)
            {
                rows.push(self.file_row(label, id, field, prompt, cx));
            }
            if client_cert(self.engine) {
                rows.push(self.text_row(
                    "Passphrase",
                    "conn-tls-key-passphrase",
                    Field::KeyPassphrase,
                    cx,
                ));
            }
        }
        rows
    }

    // A flag key as a checkbox labelled by its descriptor, `conn-<key with dashes>`; the
    // only flag key so far is `consistent_read`, whose state the form holds.
    fn flag_row(&self, f: &FormField, cx: &mut Context<Self>) -> AnyElement {
        let id = SharedString::from(format!("conn-{}", f.key.replace('_', "-")));
        let flag = Checkbox::new(id)
            .small()
            .label(f.label)
            .checked(self.consistent_read)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.consistent_read = *checked;
                this.touched(cx);
                cx.notify();
            }));
        Self::row("", flag, cx)
    }

    // The rows of one descriptor section, in order; a Port joins the field before it.
    fn section_rows(
        &self,
        fields: &[FormField],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let mut ix = 0;
        while ix < fields.len() {
            let f = &fields[ix];
            let next_is_port = fields
                .get(ix + 1)
                .is_some_and(|n| matches!(n.kind, FieldKind::Port { .. }));
            let mut shown = vec![f];
            match (f.kind, field_of(f.key)) {
                (FieldKind::Secret, _) => rows.extend(self.secret_rows(f.label, window, cx)),
                (FieldKind::Tls, _) => rows.extend(self.tls_rows(f.label, window, cx)),
                (FieldKind::Flag, _) => rows.push(self.flag_row(f, cx)),
                (FieldKind::Text, Some((field, id))) if next_is_port => {
                    rows.push(self.host_port_row(f.label, id, field, cx));
                    shown.push(&fields[ix + 1]);
                    ix += 1;
                }
                (FieldKind::Path, Some((field, id))) => {
                    let prompt = match self.engine {
                        Engine::Duckdb => "Choose DuckDB file",
                        Engine::Postgres
                        | Engine::Sqlite
                        | Engine::Mysql
                        | Engine::Mssql
                        | Engine::Mongodb
                        | Engine::Surrealdb
                        | Engine::Redis
                        | Engine::Dynamodb
                        | _ => "Choose SQLite file",
                    };
                    rows.push(self.file_row(f.label, id, field, prompt, cx));
                }
                (_, Some((field, id))) => {
                    rows.push(self.text_row(f.label, id, field, cx));
                    if field == Field::Database && !self.database_suggestions.is_empty() {
                        let items = self.database_suggestions.clone();
                        let list = self.suggestion_list(
                            "conn-database-suggestion",
                            Field::Database,
                            items,
                            Self::database_picked,
                            cx,
                        );
                        rows.push(list.into_any_element());
                    }
                }
                _ => {}
            }
            for f in shown {
                if let Some(says) = f.help {
                    let id = SharedString::from(format!("conn-{}-help", f.key.replace('_', "-")));
                    rows.push(Self::help_line(id, says, cx));
                }
            }
            ix += 1;
        }
        rows
    }

    // Values are code-like (hosts, paths, variables), so every input but Name reads in mono.
    fn styled(input: Input, mono: bool, cx: &App) -> Input {
        let input = input.small().min_h(px(32.)).bg(theme::editor_fill(cx));
        if mono {
            input.font_family(cx.theme().mono_font_family.clone())
        } else {
            input
        }
    }

    // A list under `field`'s input, aligned with it, that scrolls past MAX_SUGGESTIONS rows;
    // a click takes the row's text and closes the list (`set_value` emits no change, so
    // `picked` does what typing would have).
    fn suggestion_list(
        &self,
        id: &'static str,
        field: Field,
        items: Vec<String>,
        picked: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (border, bg, hover, radius) =
            (theme.border, theme.popover, theme.list_hover, theme.radius);
        let rows = items.into_iter().enumerate().map(|(ix, text)| {
            div()
                .id((id, ix))
                .aria_label(SharedString::from(text.clone()))
                .test_support()
                .h(px(SUGGESTION_HEIGHT))
                .flex_none()
                .px_2()
                .flex()
                .items_center()
                .truncate()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(text.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let input = this.input(field).clone();
                    input.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
                    picked(this, cx);
                    cx.notify();
                }))
        });
        div().pl(px(LABEL_WIDTH + 12.)).child(
            v_flex()
                .id(SharedString::from(format!("{id}s")))
                .test_support()
                .py_0p5()
                .border_1()
                .border_color(border)
                .rounded(radius)
                .bg(bg)
                .max_h(px(SUGGESTION_HEIGHT * MAX_SUGGESTIONS as f32 + 4.))
                .overflow_y_scroll()
                .children(rows),
        )
    }

    // Without a keychain the password lasts only this run; the note names the variable that
    // would keep it, as the typed name currently spells it.
    fn keychain_note(&self, cx: &App) -> AnyElement {
        let name = self.name.read(cx).value().trim().to_owned();
        let says = format!(
            "No keychain: the password is kept until Dbento quits. Set {} to keep it.",
            secret_env_var(&name)
        );
        div()
            .id("conn-keychain-note")
            .aria_label(SharedString::from(says.clone()))
            .test_support()
            .pl(px(LABEL_WIDTH + 12.))
            .text_xs()
            .text_color(theme::dim(cx))
            .child(says)
            .into_any_element()
    }

    // A keychain connection also reads DBENTO_SECRET_<NAME>; while a rename is typed and the
    // old variable is set, the note names the variable the new name will read instead.
    fn env_note(&self, cx: &App) -> Option<AnyElement> {
        let old = self.previous.as_deref()?;
        let new = self.name.read(cx).value().trim().to_owned();
        if !self.env_fallback || new.is_empty() || new == old {
            return None;
        }
        let says = format!(
            "The environment fallback changes from {} to {}.",
            secret_env_var(old),
            secret_env_var(&new)
        );
        let note = div()
            .id("conn-env-note")
            .aria_label(SharedString::from(says.clone()))
            .test_support()
            .pl(px(LABEL_WIDTH + 12.))
            .text_xs()
            .text_color(theme::dim(cx))
            .child(says);
        Some(note.into_any_element())
    }

    fn swatches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ring = cx.theme().foreground;
        let hollow = cx.theme().muted_foreground;
        let edge = cx.theme().border;
        // Every swatch is a 14 px mark inside the same 20 px slot, so "none" (a hollow ring)
        // matches the colour dots and the selection ring sits evenly around each.
        let dot = |selected: bool| {
            div()
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if selected {
                    ring
                } else {
                    gpui_kit::transparent_black()
                })
                .cursor_pointer()
        };
        let none = dot(self.color.is_none())
            .id("conn-color-none")
            .aria_label("none")
            .test_support()
            .child(
                div()
                    .id("conn-color-none-mark")
                    .test_support()
                    .size(px(14.))
                    .rounded_full()
                    .border_1()
                    .border_color(hollow),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.color = None;
                cx.notify();
            }));
        h_flex()
            .gap_1p5()
            .child(none)
            .children(SWATCHES.iter().enumerate().map(|(ix, &c)| {
                dot(self.color == Some(c))
                    .id(("conn-color", ix))
                    .aria_label(color_name(c))
                    .test_support()
                    // The hairline edge keeps pale tags visible on a light dialog.
                    .child(
                        div()
                            .id(("conn-color-mark", ix))
                            .test_support()
                            .size(px(14.))
                            .rounded_full()
                            .border_1()
                            .border_color(edge)
                            .bg(tag_color(c)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.color = Some(c);
                        cx.notify();
                    }))
            }))
    }
}

/// The Port box's width.
const PORT_WIDTH: f32 = 96.;

/// The label column's width, so every control starts on one line.
const LABEL_WIDTH: f32 = 96.;

impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = ui::segmented(
            "conn-engine",
            &[
                "Postgres",
                "SQLite",
                "MySQL",
                "DuckDB",
                "SQL Server",
                "SurrealDB",
                "MongoDB",
                "Redis",
                "DynamoDB",
            ],
            // An engine the picker does not offer highlights no option.
            ENGINES
                .iter()
                .position(|e| *e == self.engine)
                .unwrap_or(ENGINES.len()),
            cx.listener(|this, ix: &usize, window, cx| {
                this.set_engine(ENGINES[*ix], window, cx);
                this.touched(cx);
            }),
            window,
            cx,
        );
        // Each choice row sits in a flex row; the engine picker wraps within the field column.
        let mut sections: Vec<(&'static str, Vec<AnyElement>)> = vec![(
            "Connection",
            vec![
                self.text_row("Name", "conn-name", Field::Name, cx),
                Self::row("Engine", h_flex().child(engine), cx),
            ],
        )];
        // Rows join the section of the same title, so Name and Engine lead Connection and the
        // common rows end Options, which SQLite's descriptor lacks.
        let mut section = |title: &'static str, rows: Vec<AnyElement>| match sections
            .iter_mut()
            .find(|(t, _)| *t == title)
        {
            Some((_, held)) => held.extend(rows),
            None => sections.push((title, rows)),
        };
        let form = dbento_core::connection_form(self.engine);
        for s in &form.sections {
            section(s.title, self.section_rows(&s.fields, window, cx));
        }
        let read_only = Checkbox::new("conn-read-only")
            .small()
            .label("Read only")
            .checked(self.read_only)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.read_only = *checked;
                // Read only shapes the session, not which databases the server holds.
                this.untested(cx);
                cx.notify();
            }));
        let mut options = vec![Self::row("Safety", read_only, cx)];
        if let Some(says) = form.read_only_help {
            options.push(Self::help_line("conn-read-only-help".into(), says, cx));
        }
        options.push(self.text_row("Timeout", "conn-timeout", Field::Timeout, cx));
        let swatches = self.swatches(cx).into_any_element();
        options.push(Self::row("Colour", swatches, cx));
        options.push(self.text_row("Folder", "conn-folder", Field::Folder, cx));
        if !self.suggestions.is_empty() {
            let items = self.suggestions.iter().map(|p| p.to_string()).collect();
            let picked: fn(&mut Self, &mut Context<Self>) = |this, _| this.suggestions.clear();
            let list =
                self.suggestion_list("conn-folder-suggestion", Field::Folder, items, picked, cx);
            options.push(list.into_any_element());
        }
        section("Options", options);

        let danger = cx.theme().danger;
        let error = self.error.clone().map(|says| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id("conn-error")
                        .aria_label(SharedString::from(says.clone()))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(danger)
                        .child(says),
                )
                .when(self.stale, |row| {
                    row.child(
                        ui::secondary(Button::new("conn-reload"))
                            .small()
                            .label("Reload")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ConnectionFormEvent::Reload)),
                            ),
                    )
                })
        });
        let result = match &self.test {
            TestState::Idle => None,
            TestState::Testing => Some(("Testing\u{2026}".to_owned(), theme::dim(cx))),
            TestState::Connected => Some(("Connected".to_owned(), cx.theme().success)),
            TestState::Failed(why) => Some((format!("Failed: {why}"), danger)),
        };
        let result = result.map(|(says, color)| {
            div()
                .id("conn-test-result")
                .aria_label(SharedString::from(says.clone()))
                .test_support()
                .text_sm()
                .text_color(color)
                .child(says)
        });
        let delete = self.previous.is_some().then(|| {
            Button::new("conn-delete")
                .h(px(32.))
                .danger()
                .label("Delete")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionFormEvent::Delete)))
        });
        let cancel = ui::secondary(Button::new("conn-cancel"))
            .h(px(32.))
            .label("Cancel")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionFormEvent::Cancel)));
        let save = Button::new("conn-save")
            .h(px(32.))
            .primary()
            .label("Save")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionFormEvent::Save)));
        let save_keys = keys_for(&SaveConnection, cx).map(|k| chip_keys(&k));
        let footer = confirm::footer(cx)
            .children(delete)
            .child(
                ui::secondary(Button::new("conn-test"))
                    .h(px(32.))
                    .label("Test")
                    .disabled(self.test == TestState::Testing)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionFormEvent::Test))),
            )
            .child(div().flex_1())
            .child(with_keys(cancel, Some("Esc"), ChoiceStyle::Neutral, cx))
            .child(with_keys(
                save,
                save_keys.as_deref(),
                ChoiceStyle::Primary,
                cx,
            ));

        // Sections stack under their headings with a hairline between each pair.
        let border = cx.theme().border;
        let sections = sections
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .enumerate()
            .map(|(ix, (label, rows))| {
                v_flex()
                    .gap_1p5()
                    .when(ix > 0, |el| el.pt_3().border_t_1().border_color(border))
                    .child(ui::section_label(label, cx).pb_0p5())
                    .children(rows)
            });
        v_flex()
            .id("conn-dialog")
            .test_support()
            .key_context("ConnectionForm")
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|_, _: &SaveConnection, _, cx| cx.emit(ConnectionFormEvent::Save)),
            )
            .w_full()
            .flex_1()
            .min_h_0()
            .text_sm()
            // Only the body scrolls, so a short window keeps the footer in view.
            .child(
                v_flex()
                    .id("conn-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .children(sections)
                    .children(error)
                    // Driver errors run long, so the test outcome wraps on its own row.
                    .children(result),
            )
            .child(footer)
    }
}

/// The text and choices each control shows for a spec.
struct Shown {
    path: String,
    host: String,
    port: String,
    uri: String,
    namespace: String,
    database: String,
    user: String,
    env_var: String,
    tls_ca: String,
    tls_cert: String,
    tls_key: String,
    read_only_user: String,
    auth_level: String,
    region: String,
    endpoint: String,
    access_key_id: String,
    timeout: String,
    folder: String,
    secret: SecretChoice,
    tls: TlsChoice,
    read_only: bool,
    consistent_read: bool,
}

impl Shown {
    // The text a field shows; the password and passphrase are never shown back.
    fn value(&self, field: Field) -> String {
        match field {
            Field::Path => &self.path,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Uri => &self.uri,
            Field::Namespace => &self.namespace,
            Field::Database => &self.database,
            Field::User => &self.user,
            Field::EnvVar => &self.env_var,
            Field::TlsCa => &self.tls_ca,
            Field::TlsCert => &self.tls_cert,
            Field::TlsKey => &self.tls_key,
            Field::ReadOnlyUser => &self.read_only_user,
            Field::AuthLevel => &self.auth_level,
            Field::Region => &self.region,
            Field::Endpoint => &self.endpoint,
            Field::AccessKeyId => &self.access_key_id,
            Field::Timeout => &self.timeout,
            Field::Folder => &self.folder,
            Field::Name | Field::Password | Field::KeyPassphrase => "",
        }
        .to_owned()
    }

    fn of(spec: &ConnectionSpec) -> Self {
        let text = |v: &Option<String>| v.clone().unwrap_or_default();
        let (secret, env_var) = match &spec.secret {
            Some(SecretSource::Env(var)) => (SecretChoice::Env, var.clone()),
            Some(SecretSource::None) => (SecretChoice::None, String::new()),
            _ => (SecretChoice::Keychain, String::new()),
        };
        Shown {
            path: text(&spec.path),
            host: text(&spec.host),
            port: spec.port.map(|p| p.to_string()).unwrap_or_default(),
            uri: text(&spec.uri),
            namespace: text(&spec.namespace),
            database: text(&spec.database),
            user: text(&spec.user),
            env_var,
            tls_ca: text(&spec.tls_ca),
            tls_cert: text(&spec.tls_cert),
            tls_key: text(&spec.tls_key),
            read_only_user: text(&spec.read_only_user),
            auth_level: spec
                .auth_level
                .map(|l| l.name().to_owned())
                .unwrap_or_default(),
            region: text(&spec.region),
            endpoint: text(&spec.endpoint),
            access_key_id: text(&spec.access_key_id),
            timeout: text(&spec.statement_timeout),
            folder: spec
                .folder
                .as_ref()
                .map(|f| f.to_string())
                .unwrap_or_default(),
            secret,
            tls: match spec.tls {
                None => TlsChoice::Default,
                Some(TlsMode::Off) => TlsChoice::Off,
                Some(_) => TlsChoice::Verify,
            },
            read_only: spec.read_only.unwrap_or(false),
            consistent_read: spec.consistent_read.unwrap_or(false),
        }
    }
}

/// The engines of `conn-engine`, in option order.
const ENGINES: [Engine; 9] = [
    Engine::Postgres,
    Engine::Sqlite,
    Engine::Mysql,
    Engine::Duckdb,
    Engine::Mssql,
    Engine::Surrealdb,
    Engine::Mongodb,
    Engine::Redis,
    Engine::Dynamodb,
];

fn descriptor_fields(engine: Engine) -> Vec<FormField> {
    dbento_core::connection_form(engine)
        .sections
        .into_iter()
        .flat_map(|s| s.fields)
        .collect()
}

// The text field and element id behind a plain descriptor key.
fn field_of(key: &str) -> Option<(Field, &'static str)> {
    Some(match key {
        "path" => (Field::Path, "conn-path"),
        "host" => (Field::Host, "conn-host"),
        "port" => (Field::Port, "conn-port"),
        "uri" => (Field::Uri, "conn-uri"),
        "namespace" => (Field::Namespace, "conn-namespace"),
        "database" => (Field::Database, "conn-database"),
        "user" => (Field::User, "conn-user"),
        "read_only_user" => (Field::ReadOnlyUser, "conn-read-only-user"),
        "auth_level" => (Field::AuthLevel, "conn-auth-level"),
        "region" => (Field::Region, "conn-region"),
        "endpoint" => (Field::Endpoint, "conn-endpoint"),
        "access_key_id" => (Field::AccessKeyId, "conn-access-key-id"),
        _ => return None,
    })
}

// The inputs a descriptor field owns: the secret and TLS kinds own several.
fn fields_owned(f: &FormField) -> Vec<Field> {
    match f.kind {
        FieldKind::Secret => vec![Field::Password, Field::EnvVar],
        FieldKind::Tls => vec![
            Field::TlsCa,
            Field::TlsCert,
            Field::TlsKey,
            Field::KeyPassphrase,
        ],
        // A flag is a checkbox, not an input, so it owns none.
        FieldKind::Flag => Vec::new(),
        FieldKind::Text | FieldKind::Port { .. } | FieldKind::Path | FieldKind::Uri | _ => {
            field_of(f.key)
                .map(|(field, _)| field)
                .into_iter()
                .collect()
        }
    }
}

// Drops every key the spec's engine does not show, so a switched spec validates.
fn clear_other_engine(spec: &mut ConnectionSpec) {
    let fields = descriptor_fields(spec.engine);
    let has = |key: &str| fields.iter().any(|f| f.key == key);
    for (key, slot) in [
        ("path", &mut spec.path),
        ("host", &mut spec.host),
        ("uri", &mut spec.uri),
        ("namespace", &mut spec.namespace),
        ("database", &mut spec.database),
        ("user", &mut spec.user),
        ("read_only_user", &mut spec.read_only_user),
        ("region", &mut spec.region),
        ("endpoint", &mut spec.endpoint),
        ("access_key_id", &mut spec.access_key_id),
    ] {
        if !has(key) {
            *slot = None;
        }
    }
    if !has("port") {
        spec.port = None;
    }
    if !has("auth_level") {
        spec.auth_level = None;
    }
    if !has("consistent_read") {
        spec.consistent_read = None;
    }
    if !has("secret") {
        spec.secret = None;
    }
    if !has("tls") {
        spec.tls = None;
        (spec.tls_ca, spec.tls_cert, spec.tls_key) = (None, None, None);
    }
    if !client_cert(spec.engine) {
        (spec.tls_cert, spec.tls_key) = (None, None);
    }
}

// Whether the engine takes a client certificate and key: SQL Server and SurrealDB refuse both,
// and DynamoDB has no TLS key at all.
fn client_cert(engine: Engine) -> bool {
    match engine {
        Engine::Mssql | Engine::Surrealdb | Engine::Dynamodb => false,
        Engine::Postgres
        | Engine::Sqlite
        | Engine::Mysql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Redis
        | _ => true,
    }
}

// One of the three level names, in any case; trimmed already by the caller.
fn parse_auth_level(text: &str) -> Result<AuthLevel, String> {
    let lower = text.to_lowercase();
    [AuthLevel::Root, AuthLevel::Namespace, AuthLevel::Database]
        .into_iter()
        .find(|l| l.name() == lower)
        .ok_or_else(|| "Auth level must be root, namespace or database".to_owned())
}

// Digits only: `u16::from_str` also takes a leading `+`.
fn parse_port(text: &str) -> Result<u16, String> {
    Some(text)
        .filter(|t| t.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|t| t.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| "Port must be a number from 1 to 65535".to_owned())
}
