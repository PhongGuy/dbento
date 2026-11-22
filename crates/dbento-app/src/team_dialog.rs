//! The team dialog: sign in to a Potluck server, then pick one of its workspaces and clone it
//! into a new folder. In sign-in mode it only renews the session of a bound folder. Every call
//! that touches the token store or the network runs on the background executor.

use std::path::PathBuf;
use std::sync::Arc;

use dbento_core::{
    ApiError, Credential, LoginError, RemoteWorkspace, ServerUrl, team_join, team_login,
    team_workspaces,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};

use crate::confirm;
use crate::explorer::part_id;
use crate::team::TeamHooks;
use crate::theme;
use crate::ui;
use crate::update_banner;

/// The text a refused user name and password shows; core's own names the CLI command.
const REFUSED: &str = "The server did not accept that user name and password.";
const LABEL_WIDTH: f32 = 96.;

/// A text field of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamField {
    /// The server address.
    Server,
    /// The user name.
    User,
    /// The password, masked.
    Password,
    /// The folder a joined workspace is cloned into.
    Folder,
}

/// Where the dialog stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamStep {
    /// Server, user name and password.
    Credentials,
    /// The account's workspaces and the target folder.
    Workspaces,
}

/// What the dialog is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamDialogMode {
    /// Sign in, then clone a workspace into a folder.
    Join,
    /// Sign a bound folder's account back in; the server and user are fixed.
    SignIn {
        /// The binding's server.
        server: String,
        /// The binding's user name.
        username: String,
    },
}

/// What the dialog asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamDialogEvent {
    /// The workspace was cloned into and bound at this folder.
    Joined(PathBuf),
    /// The account is signed in again.
    SignedIn,
    /// Closed without a result.
    Close,
}

// The account a sign-in established, which the list and the join use.
struct Account {
    server: ServerUrl,
    username: String,
}

/// The sign-in and join form.
pub struct TeamDialog {
    mode: TeamDialogMode,
    hooks: TeamHooks,
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    folder: Entity<InputState>,
    insecure_ok: bool,
    step: TeamStep,
    account: Option<Account>,
    list: Vec<RemoteWorkspace>,
    selected: Option<usize>,
    error: Option<String>,
    busy: bool,
    task: Option<Task<()>>,
    focus: FocusHandle,
    _typing: Vec<Subscription>,
}

impl EventEmitter<TeamDialogEvent> for TeamDialog {}

impl Focusable for TeamDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TeamDialog {
    /// A dialog on its first step; in sign-in mode the server and user come filled.
    pub fn new(
        mode: TeamDialogMode,
        hooks: TeamHooks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (server, user) = match &mode {
            TeamDialogMode::Join => (String::new(), String::new()),
            TeamDialogMode::SignIn { server, username } => (server.clone(), username.clone()),
        };
        let mut text = |value: String, hint: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(hint)
            })
        };
        let server = text(server, "https://team.example.com");
        let user = text(user, "");
        let folder = text(String::new(), "/path/to/a/new/folder");
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        // Typing redraws the form, so the plain-http tick and the buttons follow the text.
        let _typing = [&server, &user, &password, &folder]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        TeamDialog {
            mode,
            hooks,
            server,
            user,
            password,
            folder,
            insecure_ok: false,
            step: TeamStep::Credentials,
            account: None,
            list: Vec::new(),
            selected: None,
            error: None,
            busy: false,
            task: None,
            focus: cx.focus_handle(),
            _typing,
        }
    }

    /// The input behind `field`.
    pub fn input(&self, field: TeamField) -> &Entity<InputState> {
        match field {
            TeamField::Server => &self.server,
            TeamField::User => &self.user,
            TeamField::Password => &self.password,
            TeamField::Folder => &self.folder,
        }
    }

    /// What the dialog is for.
    pub fn mode(&self) -> &TeamDialogMode {
        &self.mode
    }

    /// The step it shows.
    pub fn step(&self) -> TeamStep {
        self.step
    }

    /// The last failure, shown under the form.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Every field the step needs is filled, and a remote plain-http server is ticked.
    pub fn can_sign_in(&self, cx: &App) -> bool {
        let filled = |input: &Entity<InputState>| !input.read(cx).value().trim().is_empty();
        match self.step {
            TeamStep::Credentials => {
                filled(&self.server)
                    && filled(&self.user)
                    && filled(&self.password)
                    && (self.insecure_ok || !self.insecure(cx))
            }
            TeamStep::Workspaces => self.selected.is_some() && self.target(cx).is_some(),
        }
    }

    /// A sign-in or join call is in flight.
    pub fn signing_in(&self) -> bool {
        self.busy
    }

    // Read from the text each time, since a programmatic `set_value` emits no change.
    fn insecure(&self, cx: &App) -> bool {
        ServerUrl::parse(self.server.read(cx).value().trim()).is_ok_and(|u| u.is_insecure_http())
    }

    // The join target: only an absolute path, so the clone never lands relative to the cwd.
    fn target(&self, cx: &App) -> Option<PathBuf> {
        let path = PathBuf::from(self.folder.read(cx).value().trim());
        path.is_absolute().then_some(path)
    }

    // Signs in off the UI thread; in join mode the same job then fetches the workspace list.
    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.step != TeamStep::Credentials || !self.can_sign_in(cx) {
            return;
        }
        let server = match ServerUrl::parse(self.server.read(cx).value().trim()) {
            Ok(url) => url,
            Err(e) => return self.fail(e.to_string(), cx),
        };
        let allow_insecure = self.insecure_ok && server.is_insecure_http();
        let username = self.user.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        let tokens = Arc::clone(self.hooks.tokens());
        let paths = self.hooks.paths().clone();
        let join = self.mode == TeamDialogMode::Join;
        let job = cx.background_spawn(async move {
            let record = team_login(
                tokens.as_ref(),
                &paths,
                &server,
                &username,
                Credential::Password(password),
                allow_insecure,
            )
            .map_err(|e| login_error(&e))?;
            if !join {
                return Ok(None);
            }
            let list = team_workspaces(tokens.as_ref(), &paths, &server, Some(&record.username))
                .map_err(|e| e.to_string())?;
            let account = Account {
                server,
                username: record.username,
            };
            Ok(Some((account, list)))
        });
        self.begin(cx);
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let got = job.await;
            let _ = this.update_in(cx, |d, window, cx| d.signed_in(got, window, cx));
        }));
    }

    fn signed_in(
        &mut self,
        got: Result<Option<(Account, Vec<RemoteWorkspace>)>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match got {
            Err(e) => self.fail(e, cx),
            Ok(signed) => {
                // The token is kept; the typed password has no further use.
                self.password
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.error = None;
                match signed {
                    None => cx.emit(TeamDialogEvent::SignedIn),
                    Some((account, list)) => {
                        self.account = Some(account);
                        self.list = list;
                        self.selected = None;
                        self.step = TeamStep::Workspaces;
                    }
                }
                cx.notify();
            }
        }
    }

    // Clones the picked workspace into the folder off the UI thread and reports the folder.
    fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.step != TeamStep::Workspaces || !self.can_sign_in(cx) {
            return;
        }
        let (Some(account), Some(picked), Some(folder)) = (
            self.account.as_ref(),
            self.selected.and_then(|ix| self.list.get(ix)),
            self.target(cx),
        ) else {
            return;
        };
        let full_name = format!("{}/{}", picked.team, picked.slug);
        let (server, username) = (account.server.clone(), account.username.clone());
        let tokens = Arc::clone(self.hooks.tokens());
        let paths = self.hooks.paths().clone();
        let git = self.hooks.git_options().clone();
        let job = cx.background_spawn(async move {
            team_join(
                tokens.as_ref(),
                &paths,
                &server,
                Some(&username),
                &full_name,
                &folder,
                &git,
            )
            .map(|_| folder)
            .map_err(|e| e.to_string())
        });
        self.begin(cx);
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let got = job.await;
            let _ = this.update(cx, |d, cx| {
                d.busy = false;
                match got {
                    Ok(folder) => cx.emit(TeamDialogEvent::Joined(folder)),
                    Err(e) => d.fail(e, cx),
                }
            });
        }));
    }

    // Marks a call in flight; its task is kept on the dialog, so closing drops the wait.
    fn begin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.step = TeamStep::Credentials;
        self.account = None;
        self.list.clear();
        self.selected = None;
        self.error = None;
        cx.notify();
    }

    fn row(label: &'static str, id: &'static str, control: Input, cx: &App) -> AnyElement {
        let control = control.small().min_h(px(32.)).bg(theme::editor_fill(cx));
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
            .child(
                div()
                    .id(id)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .child(control),
            )
            .into_any_element()
    }

    fn credential_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let fixed = matches!(self.mode, TeamDialogMode::SignIn { .. });
        let mut rows = vec![
            Self::row(
                "Server",
                "team-server",
                Input::new(&self.server).disabled(fixed),
                cx,
            ),
            Self::row(
                "User name",
                "team-user",
                Input::new(&self.user).disabled(fixed),
                cx,
            ),
            Self::row(
                "Password",
                "team-password",
                Input::new(&self.password).mask_toggle(),
                cx,
            ),
        ];
        if self.insecure(cx) {
            let this = cx.weak_entity();
            rows.push(
                h_flex()
                    .pl(px(LABEL_WIDTH + 12.))
                    .child(
                        Checkbox::new("team-insecure")
                            .label("Send the password over plain http to this server")
                            .checked(self.insecure_ok)
                            .on_click(move |checked: &bool, _, cx| {
                                let on = *checked;
                                let _ = this.update(cx, |d, cx| {
                                    d.insecure_ok = on;
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn workspace_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (hover, active) = (theme.list_hover, theme.list_active);
        let mut rows: Vec<AnyElement> = self
            .list
            .iter()
            .enumerate()
            .map(|(ix, w)| {
                let says = SharedString::from(format!("{} ({})", w.name, w.role));
                let picked = self.selected == Some(ix);
                div()
                    .id(part_id("team-workspace", &format!("{}/{}", w.team, w.slug)))
                    .role(Role::ListBoxOption)
                    .aria_label(says.clone())
                    .aria_selected(picked)
                    .test_support()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(picked, |el| el.bg(active))
                    .hover(move |s| s.bg(hover))
                    .child(says)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(ix);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() {
            rows.push(
                div()
                    .text_color(theme::dim(cx))
                    .child("This account has no workspaces on the server.")
                    .into_any_element(),
            );
        }
        rows.push(Self::row(
            "Folder",
            "team-folder",
            Input::new(&self.folder),
            cx,
        ));
        rows
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = self.can_sign_in(cx) && !self.busy;
        let cancel = ui::secondary(Button::new("team-cancel"))
            .h(px(32.))
            .label("Cancel")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Close)));
        let footer = confirm::footer(cx).child(div().flex_1());
        match self.step {
            TeamStep::Credentials => footer.child(cancel).child(
                Button::new("team-sign-in")
                    .h(px(32.))
                    .primary()
                    .label(if self.busy {
                        "Signing in\u{2026}"
                    } else {
                        "Sign in"
                    })
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx))),
            ),
            TeamStep::Workspaces => footer
                .child(
                    ui::secondary(Button::new("team-back"))
                        .h(px(32.))
                        .label("Back")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
                .child(cancel)
                .child(
                    Button::new("team-join")
                        .h(px(32.))
                        .primary()
                        .label(if self.busy { "Joining\u{2026}" } else { "Join" })
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.join(window, cx))),
                ),
        }
    }
}

// A refused user name and password reads as such; any other failure keeps core's text.
fn login_error(e: &LoginError) -> String {
    if matches!(e, LoginError::Api(ApiError::Unauthenticated)) {
        REFUSED.to_owned()
    } else {
        e.to_string()
    }
}

impl Render for TeamDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = match self.step {
            TeamStep::Credentials => self.credential_rows(cx),
            TeamStep::Workspaces => self.workspace_rows(cx),
        };
        let danger = cx.theme().danger;
        let error = self
            .error
            .clone()
            .map(|says| update_banner::label("team-error", says, danger));
        let footer = self.footer(cx);
        v_flex()
            .id("team-dialog")
            .role(Role::Dialog)
            .aria_label("Team server")
            .test_support()
            .track_focus(&self.focus)
            .w_full()
            .text_sm()
            .child(
                v_flex()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .children(rows)
                    .children(error),
            )
            .child(footer)
    }
}
