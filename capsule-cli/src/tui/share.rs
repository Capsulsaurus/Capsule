//! The share-links screen: issue a view-only link over an album or one asset, and revoke one by
//! its opaque id, as `capsule share create` and `capsule share revoke` do.
//!
//! The scriptable twins are `capsule share create --json` and `capsule share revoke --json`;
//! both run [`crate::share::create`] and [`crate::share::revoke`], so the custody check (every
//! covered original already on the server) and the scope check hold here exactly as there.

use std::path::PathBuf;

use capsule_core::sharing::ShareScope;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use super::form::{Field, Form, FormKey, FormOutcome, Secret, Status};
use super::state::Effect;
use crate::share::{describe_error, validate_opaque_id};

/// One link to issue, as the form collected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateLinkRequest {
    /// The library whose keys open the covered assets.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
    /// What the link grants.
    pub scope: ShareScope,
    /// When the link stops opening, as typed (RFC 3339); the port checks it.
    pub expires_at: Option<String>,
    /// The passphrase a recipient must also enter, for a protected link.
    pub link_passphrase: Option<Secret>,
}

/// A link the server registered.
///
/// The URL carries the link's fragment secret, so it is a [`Secret`]: drawn on screen, where
/// the person copies it, and never written to the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedLink {
    /// The opaque id `capsule share revoke` takes.
    pub opaque_id: String,
    /// The URL a recipient opens.
    pub url: Secret,
    /// How many assets the link covers.
    pub covered: usize,
    /// Whether the recipient also needs the link passphrase.
    pub passphrase_protected: bool,
    /// The normalized expiry, if any.
    pub expires_at: Option<String>,
}

impl From<crate::share::IssuedShare> for IssuedLink {
    fn from(issued: crate::share::IssuedShare) -> Self {
        Self {
            opaque_id: issued.opaque_id,
            url: Secret::new(issued.url),
            covered: issued.covered,
            passphrase_protected: issued.passphrase_protected,
            expires_at: issued.expires_at,
        }
    }
}

/// What the share-links screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareAction {
    /// Show the issue form and start typing into it.
    BeginCreate,
    /// Show the revoke form and start typing into it.
    BeginRevoke,
    /// [`ShareEffect::Create`] finished.
    Created(Result<IssuedLink, String>),
    /// [`ShareEffect::Revoke`] finished: the revoked link's opaque id.
    Revoked(Result<String, String>),
}

/// The work the share-links screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareEffect {
    /// Issue a link; answered by [`ShareAction::Created`].
    Create(CreateLinkRequest),
    /// Revoke a link; answered by [`ShareAction::Revoked`].
    Revoke {
        /// The link's opaque id, already checked for shape.
        opaque_id: String,
    },
}

impl ShareEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> ShareAction {
        match self {
            Self::Create(_) => ShareAction::Created(Err(reason)),
            Self::Revoke { .. } => ShareAction::Revoked(Err(reason)),
        }
    }
}

/// Which form the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    /// Issue a link.
    Create,
    /// Revoke a link.
    Revoke,
}

const LIBRARY: usize = 0;
const PASSPHRASE: usize = 1;
const ALBUM: usize = 2;
const ASSET: usize = 3;
const EXPIRES: usize = 4;
const LINK_PASSPHRASE: usize = 5;
const OPAQUE_ID: usize = 0;

/// The share-links screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareScreen {
    mode: ShareMode,
    create: Form,
    revoke: Form,
    editing: bool,
    status: Status,
    issued: Option<IssuedLink>,
}

impl Default for ShareScreen {
    fn default() -> Self {
        Self {
            mode: ShareMode::Create,
            create: Form::new(vec![
                Field::text("Library"),
                Field::secret("Library passphrase"),
                Field::text("Album id"),
                Field::text("Asset id"),
                Field::text("Expires at (optional)"),
                Field::secret("Link passphrase (optional)"),
            ]),
            revoke: Form::new(vec![Field::text("Opaque id")]),
            editing: false,
            status: Status::Idle,
            issued: None,
        }
    }
}

impl ShareScreen {
    /// Whether keys go into the form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Which form is shown.
    #[must_use]
    pub fn mode(&self) -> ShareMode {
        self.mode
    }

    /// The form on screen.
    #[must_use]
    pub fn form(&self) -> &Form {
        match self.mode {
            ShareMode::Create => &self.create,
            ShareMode::Revoke => &self.revoke,
        }
    }

    fn form_mut(&mut self) -> &mut Form {
        match self.mode {
            ShareMode::Create => &mut self.create,
            ShareMode::Revoke => &mut self.revoke,
        }
    }

    /// Where the last request stands.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The last link issued this session.
    #[must_use]
    pub fn issued(&self) -> Option<&IssuedLink> {
        self.issued.as_ref()
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: ShareAction) -> Vec<Effect> {
        match action {
            ShareAction::BeginCreate | ShareAction::BeginRevoke => {
                if !self.status.is_working() {
                    self.mode = if action == ShareAction::BeginCreate {
                        ShareMode::Create
                    } else {
                        ShareMode::Revoke
                    };
                    self.editing = true;
                    self.status = Status::Idle;
                }
            }
            ShareAction::Created(Ok(issued)) => {
                self.status = Status::Done(match issued.covered {
                    1 => "Share link issued for 1 asset.".to_owned(),
                    count => format!("Share link issued for {count} assets."),
                });
                // Revoking it is the likeliest next step: its id is ready in the revoke form.
                self.revoke.set(OPAQUE_ID, &issued.opaque_id);
                self.issued = Some(issued);
            }
            ShareAction::Created(Err(reason)) => {
                self.status = Status::Failed(format!("Share link not issued: {reason}"));
            }
            ShareAction::Revoked(Ok(id)) => {
                if self.issued.as_ref().is_some_and(|i| i.opaque_id == id) {
                    self.issued = None;
                }
                self.status = Status::Done(format!("Share link {id} no longer opens for anyone."));
            }
            ShareAction::Revoked(Err(reason)) => {
                self.status = Status::Failed(format!("Share link not revoked: {reason}"));
            }
        }
        Vec::new()
    }

    /// Apply a key typed into the form.
    pub fn on_form_key(&mut self, key: FormKey) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        match self.form_mut().apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.editing = false;
                Vec::new()
            }
            FormOutcome::Submitted => {
                if self.status.is_working() {
                    return Vec::new();
                }
                let effect = match self.mode {
                    ShareMode::Create => self.create_request().map(ShareEffect::Create),
                    ShareMode::Revoke => validate_opaque_id(self.revoke.value(OPAQUE_ID))
                        .map(|opaque_id| ShareEffect::Revoke { opaque_id })
                        .map_err(|error| describe_error(&error)),
                };
                match effect {
                    Ok(effect) => {
                        self.form_mut().clear_secrets();
                        self.editing = false;
                        self.status = Status::Working(match self.mode {
                            ShareMode::Create => "Issuing the link…",
                            ShareMode::Revoke => "Revoking the link…",
                        });
                        vec![Effect::Share(effect)]
                    }
                    Err(reason) => {
                        self.status = Status::Failed(reason);
                        Vec::new()
                    }
                }
            }
        }
    }

    /// The issue form as a request, or why it is not one.
    fn create_request(&self) -> Result<CreateLinkRequest, String> {
        let form = &self.create;
        let library = form.value(LIBRARY);
        let passphrase = form.secret(PASSPHRASE);
        if library.is_empty() || passphrase.is_empty() {
            return Err("Enter the library and its passphrase.".to_owned());
        }
        let scope = match (
            parse_id(form.value(ALBUM), "album")?,
            parse_id(form.value(ASSET), "asset")?,
        ) {
            (Some(album), None) => ShareScope::Album(album),
            (None, Some(asset)) => ShareScope::Asset(asset),
            _ => return Err("Give an album id or an asset id: exactly one.".to_owned()),
        };
        let expires_at = Some(form.value(EXPIRES).to_owned()).filter(|v| !v.is_empty());
        let link_passphrase = Some(form.secret(LINK_PASSPHRASE)).filter(|p| !p.is_empty());
        Ok(CreateLinkRequest {
            library: PathBuf::from(library),
            passphrase,
            scope,
            expires_at,
            link_passphrase,
        })
    }
}

/// An optional id field: empty is `None`, anything else must be a UUID.
fn parse_id(value: &str, what: &str) -> Result<Option<Uuid>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| format!("The {what} id {value:?} is not a UUID."))
}

/// The lines describing an issued link: the URL on a line of its own, what else the recipient
/// needs, and how to revoke it.
#[must_use]
pub fn issued_lines(issued: &IssuedLink) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Anyone with this URL can view what it covers:"),
        Line::from(issued.url.expose().to_owned().bold()),
    ];
    if issued.passphrase_protected {
        lines.push(Line::from(
            "The link is passphrase-protected: send the passphrase separately from the URL."
                .fg(Color::Yellow),
        ));
    }
    if let Some(expires_at) = &issued.expires_at {
        lines.push(Line::from(format!(
            "The link stops opening at {expires_at}."
        )));
    }
    lines.push(Line::from(format!(
        "To revoke it: x, then Enter (or `capsule share revoke {}`).",
        issued.opaque_id
    )));
    lines
}

/// Draw the share-links screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, screen: &ShareScreen, title: &'static str) {
    let mut lines = vec![
        Line::from(match screen.mode {
            ShareMode::Create => {
                "Issue a view-only link over an album or one asset. Every covered original must \
                 already be on the server (`capsule push`)."
            }
            ShareMode::Revoke => "Revoke a link by its opaque id, the 32 hex characters after /s/.",
        }),
        Line::default(),
    ];
    if screen.editing || screen.mode == ShareMode::Revoke || screen.issued.is_none() {
        lines.extend(screen.form().lines(screen.editing));
        lines.push(Line::default());
    }
    if let Some(status) = screen.status.line() {
        lines.push(status);
    }
    if let Some(issued) = &screen.issued
        && !screen.editing
    {
        lines.extend(issued_lines(issued));
    }
    lines.push(Line::from(
        "Scriptable: `capsule share create … --json`, `capsule share revoke ID --json`."
            .fg(Color::DarkGray),
    ));
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests;
