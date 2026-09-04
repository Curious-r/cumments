//! Pure event processing logic – independent of how events are received.
//!
//! This module defines the core "projection" functions that transform
//! Matrix events into local read-model updates. It does **not** depend
//! on `matrix_sdk` or any transport-specific types. The AppService
//! `PushReceiver` (and any future transport) calls into these same
//! functions.

use crate::backfill::BackfillRequest;
use crate::parsed::{
    ParsedPollVote, ParsedReaction, ParsedRoomMessage, ParsedRoomRedaction, ParsedRoomState,
    ParsedSpaceChild,
};
use crate::verification::{verify_delete_proof, verify_visitor_event};
use anyhow::Result;
use cumments_core::audit::{CommandAuditStatus, NewCommandAuditEntry};
use cumments_core::{
    collections::BoundedLruMap,
    governance::{
        MANAGER_LEVEL, MODERATOR_LEVEL, POWER_LEVELS_EVENT_TYPE, RoleEntry, SITE_ADMIN_LEVEL,
        SITE_ROLE_MIN_LEVEL, can_send_state_event, is_as_managed_user, role_entries,
        validate_governance_user_id,
    },
    identity::{post_signature_message, signature_message},
    models::{
        AuthorKind, AuthorSnapshot, Content, EditProjectionOutcome, Message,
        MessageRedactionOutcome, MessageRevision, MessageStatus, PageSlug, PollVote,
        ProjectionRepairInput, Reaction, RoomIdentity, RoomMember, RoomStateEvent, RoomStatus,
        SiteId, SubmissionCompletion, TextStyle,
    },
    ports::{
        CommandAuditStore, GovernanceStore, MatrixDriver, MessageStore, ProjectionRepairStore,
        RegistryStore, RoleClaimStore, RoomStore, SiteStore, StickerPackStore, SubmissionStore,
    },
    projector_events::ProjectorEvent,
    protocol::CLAIM_MESSAGE_PREFIX,
    rate_limit::SlidingWindowRateLimiter,
    redaction::{UnsupportedRoomVersion, redact_state_content_for_version},
    site_auth::{
        SiteAuthMode, SiteAuthPolicy, constant_time_eq, generate_token, sha256_hex, token_hash,
    },
    sticker_packs::{
        AddStickerInput, IMAGE_PACK_EVENT_TYPE, StickerPackProjection, StickerPackUseCaseError,
        add_site_sticker, list_site_sticker_packs, parse_image_pack_content, remove_site_sticker,
    },
};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tracing::{debug, error, info, instrument, warn};

// ── Core processing functions ─────────────────────────────────────

/// The central processor – holds only abstract store references.
pub struct EventProcessor {
    site_store: Arc<dyn SiteStore>,
    registry_store: Arc<dyn RegistryStore>,
    message_store: Arc<dyn MessageStore>,
    room_store: Arc<dyn RoomStore>,
    governance_store: Arc<dyn GovernanceStore>,
    sticker_pack_store: Arc<dyn StickerPackStore>,
    projection_repair_store: Arc<dyn ProjectionRepairStore>,
    role_claim_store: Arc<dyn RoleClaimStore>,
    _submission_store: Arc<dyn SubmissionStore>,
    site_service: Arc<cumments_core::site_service::SiteService>,
    driver: Option<Arc<dyn MatrixDriver>>,
    event_bus: broadcast::Sender<ProjectorEvent>,
    /// Wakes the reconciler after a site Space's power levels are projected,
    /// so client-side governance edits propagate to rooms without waiting for
    /// the periodic reconcile tick.
    projection_notify: Arc<Notify>,
    server_name: Option<String>,
    /// Per-inviter limiter for Bot auto-join (invite admission).
    /// Protects the resource cost of successful JOIN -> AS-visible event stream,
    /// not governance privilege. Shared by claim-DM and bootstrap branches.
    invite_join_limiter: SlidingWindowRateLimiter,
    /// Routes `!cumments` chat commands; kept separate so projection does not
    /// carry command-only state.
    command_router: BotCommandRouter,
    /// Set by the AppService router while an event is projected. Captured
    /// events are persisted to the SSE outbox instead of broadcast directly.
    event_capture: Mutex<Option<Vec<ProjectorEvent>>>,
}

/// Dependencies of the [`EventProcessor`], kept as one struct so the growing
/// set of stores stays readable at construction sites.
pub struct EventProcessorDeps {
    pub site_store: Arc<dyn SiteStore>,
    pub registry_store: Arc<dyn RegistryStore>,
    pub message_store: Arc<dyn MessageStore>,
    pub room_store: Arc<dyn RoomStore>,
    pub governance_store: Arc<dyn GovernanceStore>,
    pub sticker_pack_store: Arc<dyn StickerPackStore>,
    pub projection_repair_store: Arc<dyn ProjectionRepairStore>,
    pub role_claim_store: Arc<dyn RoleClaimStore>,
    pub submission_store: Arc<dyn SubmissionStore>,
    pub audit_store: Arc<dyn CommandAuditStore>,
    pub site_auth_store: Arc<dyn cumments_core::ports::SiteAuthStore>,
    /// Operator-declared site overlay, so bot commands see config-only sites.
    pub site_auth_policy: Arc<SiteAuthPolicy>,
    pub site_service: Arc<cumments_core::site_service::SiteService>,
    pub driver: Option<Arc<dyn MatrixDriver>>,
    /// Instance operators for chat commands (from `security.operator_mxids`).
    pub operator_mxids: Vec<String>,
    /// Optional bot-triggered backfill queue (set when the binary runs a
    /// backfill worker).
    pub backfill_tx: Option<mpsc::Sender<BackfillRequest>>,
    pub event_bus: broadcast::Sender<ProjectorEvent>,
    /// Wakes governance reconcile passes (retirement, role propagation)
    /// after bot-driven governance writes, mirroring the API.
    pub governance_notify: Arc<Notify>,
    pub projection_notify: Arc<Notify>,
    pub server_name: Option<String>,
}

/// Chat command router: owns the command-only state and executes `!cumments`
/// commands. Kept separate from projection so `EventProcessor` only routes.
pub struct BotCommandRouter {
    registry_store: Arc<dyn RegistryStore>,
    site_store: Arc<dyn SiteStore>,
    governance_store: Arc<dyn GovernanceStore>,
    sticker_pack_store: Arc<dyn StickerPackStore>,
    role_claim_store: Arc<dyn RoleClaimStore>,
    audit_store: Arc<dyn CommandAuditStore>,
    site_auth_store: Arc<dyn cumments_core::ports::SiteAuthStore>,
    site_auth_policy: Arc<SiteAuthPolicy>,
    site_service: Arc<cumments_core::site_service::SiteService>,
    driver: Option<Arc<dyn MatrixDriver>>,
    operator_mxids: Vec<String>,
    /// Protects the private-channel membership lookup from prefix floods.
    command_ingress: SlidingWindowRateLimiter,
    /// Authoritative per-MXID budget applied after the channel is verified.
    command_rate: SlidingWindowRateLimiter,
    active_sites: Mutex<BoundedLruMap<String>>,
    backfill_tx: Option<mpsc::Sender<BackfillRequest>>,
    governance_notify: Arc<Notify>,
}

impl BotCommandRouter {
    pub fn new(deps: &EventProcessorDeps) -> Self {
        Self {
            registry_store: deps.registry_store.clone(),
            site_store: deps.site_store.clone(),
            governance_store: deps.governance_store.clone(),
            sticker_pack_store: deps.sticker_pack_store.clone(),
            role_claim_store: deps.role_claim_store.clone(),
            audit_store: deps.audit_store.clone(),
            site_auth_store: deps.site_auth_store.clone(),
            site_auth_policy: deps.site_auth_policy.clone(),
            site_service: deps.site_service.clone(),
            driver: deps.driver.clone(),
            operator_mxids: deps.operator_mxids.clone(),
            command_ingress: SlidingWindowRateLimiter::new(
                Self::COMMAND_INGRESS_LIMIT,
                Self::COMMAND_RATE_WINDOW,
                Self::COMMAND_RATE_MAX_SENDERS,
            ),
            command_rate: SlidingWindowRateLimiter::new(
                Self::COMMAND_RATE_LIMIT,
                Self::COMMAND_RATE_WINDOW,
                Self::COMMAND_RATE_MAX_SENDERS,
            ),
            active_sites: Mutex::new(BoundedLruMap::new(Self::ACTIVE_SITES_MAX_USERS)),
            backfill_tx: deps.backfill_tx.clone(),
            governance_notify: deps.governance_notify.clone(),
        }
    }
}

/// Result of one chat command: the reply text plus the resolved site id for
/// audit purposes.
struct CommandOutcome {
    reply: String,
    site_id: Option<String>,
    invalid: bool,
}

impl CommandOutcome {
    fn plain(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            site_id: None,
            invalid: false,
        }
    }

    fn invalid(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            site_id: None,
            invalid: true,
        }
    }
}

/// A command failure with a user-facing message. `denied` marks
/// authorization rejections so the audit trail records them distinctly from
/// ordinary errors.
struct CommandError {
    message: String,
    denied: bool,
}

impl CommandError {
    fn denied(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
            denied: true,
        }
    }

    fn error(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
            denied: false,
        }
    }
}

impl From<cumments_core::site_auth::SiteServiceError> for CommandError {
    fn from(error: cumments_core::site_auth::SiteServiceError) -> Self {
        Self::error(error)
    }
}

impl From<cumments_core::management::ManagementError> for CommandError {
    fn from(error: cumments_core::management::ManagementError) -> Self {
        Self::error(error)
    }
}

impl From<StickerPackUseCaseError> for CommandError {
    fn from(error: StickerPackUseCaseError) -> Self {
        Self::error(error)
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::error(error.to_string())
    }
}

use crate::bot_commands::help_text;

/// Whether the room is a verified private channel: exactly the bot and
/// the sender are joined. Fails closed when it cannot be verified.
async fn is_private_channel(
    event: &ParsedRoomMessage,
    driver: &Option<Arc<dyn MatrixDriver>>,
) -> Result<bool> {
    let Some(driver) = driver else {
        return Ok(false);
    };
    let Some(bot) = driver.sender_user_id() else {
        return Ok(false);
    };
    let members = match driver.get_joined_members(&event.room_id).await {
        Ok(members) => members,
        Err(error) => {
            warn!(
                "private channel check failed for {}: {:#}",
                event.room_id, error
            );
            return Ok(false);
        }
    };
    Ok(members.len() == 2
        && members.iter().any(|m| m == &event.sender)
        && members.iter().any(|m| m == &bot))
}

impl BotCommandRouter {
    /// A cheap per-sender cap before the private-channel membership lookup.
    /// The later authoritative per-sender budget is tighter.
    const COMMAND_INGRESS_LIMIT: usize = 30;
    const COMMAND_RATE_LIMIT: usize = 10;
    const COMMAND_RATE_WINDOW: StdDuration = StdDuration::from_secs(60);
    const COMMAND_RATE_MAX_SENDERS: usize = 10_000;
    const ACTIVE_SITES_MAX_USERS: usize = 10_000;

    async fn reply(&self, event: &ParsedRoomMessage, body: &str) {
        if let Some(driver) = &self.driver
            && let Err(error) = driver.send_bot_message(&event.room_id, body).await
        {
            warn!("bot reply to {} failed: {:#}", event.room_id, error);
        }
    }

    async fn record_audit(
        &self,
        event: &ParsedRoomMessage,
        command: &str,
        site_id: Option<String>,
        status: CommandAuditStatus,
        error: Option<String>,
    ) {
        if let Err(e) = self
            .audit_store
            .record_command_audit(&NewCommandAuditEntry {
                actor_mxid: event.sender.clone(),
                room_id: event.room_id.clone(),
                command: command.to_string(),
                site_id,
                status,
                error,
            })
            .await
        {
            warn!("command audit write failed for {}: {:#}", event.sender, e);
        }
    }

    /// Handles `!cumments ...` messages. Returns `true` when the message was
    /// consumed as a command (and must not be projected as a comment).
    pub async fn process_bot_command(&self, event: &ParsedRoomMessage) -> Result<bool> {
        if event.is_virtual_user_sender {
            return Ok(false);
        }
        let Content::Text(text) = &event.content else {
            return Ok(false);
        };
        if text.style != TextStyle::Normal
            || event.relates_to.is_some()
            || event.reply_to.is_some()
            || event.thread_root.is_some()
        {
            return Ok(false);
        }
        let Some(rest) = text.body.trim().strip_prefix("!cumments") else {
            return Ok(false);
        };
        let line = rest.trim();

        // Prefix floods must not turn into homeserver membership reads. Keying
        // admission by sender keeps one flooder from starving administrators.
        if !self.command_ingress.allow(&event.sender) {
            debug!(
                sender = %event.sender,
                "Dropping prefix flood before private-channel check"
            );
            return Ok(true);
        }

        // Commands only act inside a verified private channel; elsewhere the
        // message is consumed silently so it never becomes a comment.
        if !is_private_channel(event, &self.driver).await? {
            debug!(
                "Ignoring !cumments from {} outside a private channel",
                event.sender
            );
            return Ok(true);
        }
        if !self.command_rate.allow(&event.sender) {
            debug!(
                sender = %event.sender,
                room_id = %event.room_id,
                "Ignoring rate-limited !cumments command"
            );
            return Ok(true);
        }
        if line.is_empty() {
            self.reply(event, &help_text()).await;
            self.record_audit(event, line, None, CommandAuditStatus::Invalid, None)
                .await;
            return Ok(true);
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        match self.run_command(event, &tokens).await {
            Ok(outcome) => {
                self.reply(event, &outcome.reply).await;
                let status = if outcome.invalid {
                    CommandAuditStatus::Invalid
                } else {
                    CommandAuditStatus::Ok
                };
                self.record_audit(event, line, outcome.site_id, status, None)
                    .await;
            }
            Err(error) => {
                let status = if error.denied {
                    CommandAuditStatus::Denied
                } else {
                    CommandAuditStatus::Error
                };
                let msg = format!("Error: {}", error.message);
                self.reply(event, &msg).await;
                self.record_audit(event, line, None, status, Some(error.message))
                    .await;
            }
        }
        Ok(true)
    }

    async fn run_command(
        &self,
        event: &ParsedRoomMessage,
        tokens: &[&str],
    ) -> Result<CommandOutcome, CommandError> {
        match tokens {
            ["help"] => Ok(CommandOutcome::plain(help_text())),
            ["sites", "list"] => {
                self.require_operator(event)?;
                let sites = cumments_core::management::list_effective_sites(
                    self.site_auth_store.as_ref(),
                    &self.site_auth_policy,
                )
                .await?;
                let reply = if sites.is_empty() {
                    "No sites.".to_string()
                } else {
                    sites
                        .iter()
                        .map(|s| {
                            if s.from_config {
                                format!("{} (config)", s.site_id)
                            } else {
                                s.site_id.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CommandOutcome::plain(reply))
            }
            ["sites", "use", id] => {
                SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let mut active = self.active_sites.lock().await;
                active.put(event.sender.clone(), id.to_string());
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Active site set to {id}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["sites", "status"] => {
                let id = self.active_site_for(event).await?;
                self.site_status(&id).await.map(|reply| CommandOutcome {
                    reply,
                    site_id: Some(id),
                    invalid: false,
                })
            }
            ["sites", "status", id] => {
                self.require_site_access(event, id).await?;
                self.site_status(id).await.map(|reply| CommandOutcome {
                    reply,
                    site_id: Some(id.to_string()),
                    invalid: false,
                })
            }
            ["sites", "register", id] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let driver = self.require_driver()?;
                cumments_core::governance::validate_governance_user_id(&event.sender)
                    .map_err(CommandError::error)?;
                let token = generate_token();
                self.site_auth_store
                    .register_site(site_id.as_str(), &token_hash(&token), true)
                    .await?;
                cumments_core::management::bootstrap_first_site_admin(
                    self.role_claim_store.as_ref(),
                    driver,
                    &self.site_service,
                    site_id.as_str(),
                    &event.sender,
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Site {id} registered. {} is now its first site admin. Claim token (shown once, do not share):\n{token}",
                        event.sender
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["managers", "add", id, mxid] => {
                self.require_site_access(event, id).await?;
                let pending = cumments_core::management::create_role_claim(
                    self.role_claim_store.as_ref(),
                    id,
                    "",
                    mxid,
                    MANAGER_LEVEL,
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Created manager claim for {mxid}. Ask them to send to the bot:\ncumments-claim:{}",
                        pending.verify_token
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["managers", "remove", id, mxid] => {
                self.require_site_access(event, id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm removal of manager {mxid} from {id}? Reply:\n!cumments managers remove {id} {mxid} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["managers", "remove", id, mxid, "--confirm"] => {
                self.require_site_access(event, id).await?;
                let driver = self.require_driver()?;
                let removal = cumments_core::management::remove_site_role(
                    self.role_claim_store.as_ref(),
                    self.governance_store.as_ref(),
                    driver,
                    &self.site_service,
                    id,
                    mxid,
                    MANAGER_LEVEL,
                )
                .await?;
                if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
                    self.governance_notify.notify_one();
                }
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Removed manager {mxid} from {id}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["managers", "resign", id] => Ok(CommandOutcome {
                invalid: false,
                reply: format!(
                    "Confirm resigning as manager of {id}? This only removes the role, it won't make you leave the room. Reply:\n!cumments managers resign {id} --confirm"
                ),
                site_id: Some(id.to_string()),
            }),
            ["managers", "resign", id, "--confirm"] => {
                let driver = self.require_driver()?;
                let removal = cumments_core::management::remove_site_role(
                    self.role_claim_store.as_ref(),
                    self.governance_store.as_ref(),
                    driver,
                    &self.site_service,
                    id,
                    &event.sender,
                    MANAGER_LEVEL,
                )
                .await?;
                if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
                    self.governance_notify.notify_one();
                }
                Ok(CommandOutcome {
                    invalid: false,
                    reply: "Resigned as manager.".to_string(),
                    site_id: Some(id.to_string()),
                })
            }
            ["moderators", "add", id, slug, mxid] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let room_id = self
                    .registry_store
                    .get_registered_room(&site_id, &page_slug)
                    .await?
                    .ok_or_else(|| {
                        CommandError::error(format!("No room registered for {id}/{slug}."))
                    })?;
                self.require_room_state_permission(event, &room_id).await?;
                let pending = cumments_core::management::create_role_claim(
                    self.role_claim_store.as_ref(),
                    id,
                    &room_id,
                    mxid,
                    MODERATOR_LEVEL,
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Created moderator claim for {mxid}. Ask them to send to the bot:\ncumments-claim:{}",
                        pending.verify_token
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["moderators", "remove", id, slug, mxid] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let room_id = self
                    .registry_store
                    .get_registered_room(&site_id, &page_slug)
                    .await?
                    .ok_or_else(|| {
                        CommandError::error(format!("No room registered for {id}/{slug}."))
                    })?;
                self.require_room_state_permission(event, &room_id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm removal of moderator {mxid} from {id}/{slug}? Reply:\n!cumments moderators remove {id} {slug} {mxid} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["moderators", "remove", id, slug, mxid, "--confirm"] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let room_id = self
                    .registry_store
                    .get_registered_room(&site_id, &page_slug)
                    .await?
                    .ok_or_else(|| {
                        CommandError::error(format!("No room registered for {id}/{slug}."))
                    })?;
                self.require_room_state_permission(event, &room_id).await?;
                let removal = cumments_core::management::remove_room_moderator(
                    self.role_claim_store.as_ref(),
                    self.governance_store.as_ref(),
                    self.require_driver()?,
                    id,
                    &room_id,
                    mxid,
                )
                .await?;
                if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
                    self.governance_notify.notify_one();
                }
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Removed moderator {mxid} from {id}/{slug}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["moderators", "resign", id, slug] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let room_id = self
                    .registry_store
                    .get_registered_room(&site_id, &page_slug)
                    .await?
                    .ok_or_else(|| {
                        CommandError::error(format!("No room registered for {id}/{slug}."))
                    })?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm resigning as moderator of {id}/{slug} ({room_id})? This only removes the role, it won't make you leave the room. Reply:\n!cumments moderators resign {id} {slug} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["moderators", "resign", id, slug, "--confirm"] => {
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let room_id = self
                    .registry_store
                    .get_registered_room(&site_id, &page_slug)
                    .await?
                    .ok_or_else(|| {
                        CommandError::error(format!("No room registered for {id}/{slug}."))
                    })?;
                let removal = cumments_core::management::remove_room_moderator(
                    self.role_claim_store.as_ref(),
                    self.governance_store.as_ref(),
                    self.require_driver()?,
                    id,
                    &room_id,
                    &event.sender,
                )
                .await?;
                if removal == cumments_core::management::RoleRemoval::AppliedRemoved {
                    self.governance_notify.notify_one();
                }
                Ok(CommandOutcome {
                    invalid: false,
                    reply: "Resigned as moderator.".to_string(),
                    site_id: Some(id.to_string()),
                })
            }
            ["pages", "upgrades", "create", id, slug, version] => {
                self.require_site_access(event, id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm upgrading {id}/{slug} to {version}? Reply:\n!cumments pages upgrades create {id} {slug} {version} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            [
                "pages",
                "upgrades",
                "create",
                id,
                slug,
                version,
                "--confirm",
            ] => {
                self.require_site_access(event, id).await?;
                let driver = self.require_driver()?;
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                let replacement = cumments_core::management::upgrade_site_page_room(
                    driver,
                    self.registry_store.as_ref(),
                    self.site_service.as_ref(),
                    &site_id,
                    &page_slug,
                    version,
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Room {id}/{slug} upgraded to {version}, new room: {replacement}"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["pages", "retirements", "create", id, slug] => {
                self.require_site_access(event, id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm retiring the comment room for {id}/{slug}? This cannot be undone. Reply:\n!cumments pages retirements create {id} {slug} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["pages", "retirements", "create", id, slug, "--confirm"] => {
                self.require_site_access(event, id).await?;
                let site_id = SiteId::new(id.to_string()).map_err(CommandError::error)?;
                let page_slug = PageSlug::new(slug.to_string()).map_err(CommandError::error)?;
                if !cumments_core::management::retire_page_room(
                    self.registry_store.as_ref(),
                    &site_id,
                    &page_slug,
                )
                .await?
                {
                    return Err(CommandError::error(format!(
                        "No active room registered for {id}/{slug}."
                    )));
                }
                self.governance_notify.notify_one();
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Comment room for {id}/{slug} marked as retired. Processing in background."
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["stickers", "list", id] => {
                self.require_site_sticker_access(event, id).await?;
                let packs = list_site_sticker_packs(self.sticker_pack_store.as_ref(), id).await?;
                let reply = if packs.is_empty() {
                    "No sticker packs.".to_string()
                } else {
                    packs
                        .iter()
                        .map(|projection| {
                            let pack = &projection.pack;
                            let images = pack
                                .content
                                .images
                                .iter()
                                .map(|image| format!("{}={}", image.shortcode, image.url))
                                .collect::<Vec<_>>()
                                .join(", ");
                            if images.is_empty() {
                                format!("{} (empty)", pack.state_key)
                            } else {
                                format!("{}: {}", pack.state_key, images)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CommandOutcome {
                    invalid: false,
                    reply,
                    site_id: Some(id.to_string()),
                })
            }
            ["stickers", "add", id, pack_id, shortcode, url] => {
                self.require_site_sticker_access(event, id).await?;
                let driver = self.require_driver()?;
                add_site_sticker(
                    self.site_store.as_ref(),
                    driver,
                    AddStickerInput {
                        site_id: id,
                        pack_id,
                        shortcode,
                        url,
                        body: None,
                        info: None,
                    },
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Added sticker {shortcode} to pack {pack_id}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["stickers", "add", id, pack_id, shortcode, url, body @ ..] => {
                self.require_site_sticker_access(event, id).await?;
                let driver = self.require_driver()?;
                add_site_sticker(
                    self.site_store.as_ref(),
                    driver,
                    AddStickerInput {
                        site_id: id,
                        pack_id,
                        shortcode,
                        url,
                        body: Some(body.join(" ")),
                        info: None,
                    },
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Added sticker {shortcode} to pack {pack_id}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["stickers", "remove", id, pack_id, shortcode] => {
                self.require_site_sticker_access(event, id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm removing sticker {shortcode} from pack {pack_id}? Reply:\n!cumments stickers remove {id} {pack_id} {shortcode} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["stickers", "remove", id, pack_id, shortcode, "--confirm"] => {
                self.require_site_sticker_access(event, id).await?;
                let driver = self.require_driver()?;
                remove_site_sticker(self.site_store.as_ref(), driver, id, pack_id, shortcode)
                    .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Removed sticker {shortcode} from pack {pack_id}."),
                    site_id: Some(id.to_string()),
                })
            }
            ["claim-tokens", "rotate", id] => {
                self.require_operator(event)?;
                let token = cumments_core::management::rotate_claim_token(
                    self.site_auth_store.as_ref(),
                    id,
                )
                .await?
                .ok_or_else(|| CommandError::error("Site not found."))?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("New claim token (shown once, do not share):\n{token}"),
                    site_id: Some(id.to_string()),
                })
            }
            ["secrets", "issue", id] => {
                self.require_site_access(event, id).await?;
                let secret =
                    cumments_core::management::issue_secret(self.site_auth_store.as_ref(), id)
                        .await?
                        .ok_or_else(|| CommandError::error("Site not found."))?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("HMAC secret (shown once, do not share):\n{secret}"),
                    site_id: Some(id.to_string()),
                })
            }
            ["retirements", "create", id] => {
                self.require_site_access(event, id).await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm retiring site {id}? This cannot be undone. Reply:\n!cumments retirements create {id} --confirm"
                    ),
                    site_id: Some(id.to_string()),
                })
            }
            ["retirements", "create", id, "--confirm"] => {
                self.require_site_access(event, id).await?;
                if !cumments_core::management::retire_site(self.site_auth_store.as_ref(), id)
                    .await?
                {
                    return Err(CommandError::error("Site not found or already retired."));
                }
                self.governance_notify.notify_one();
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Site {id} marked as retiring. Processing in background."),
                    site_id: Some(id.to_string()),
                })
            }
            ["quarantined-rooms", "list"] => {
                self.require_operator(event)?;
                let rooms = self.registry_store.get_quarantined_rooms().await?;
                let reply = if rooms.is_empty() {
                    "No quarantined rooms.".to_string()
                } else {
                    rooms
                        .iter()
                        .map(|r| format!("{} ({}/{})", r.room_id, r.site_id, r.page_slug))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CommandOutcome::plain(reply))
            }
            ["backfill"] => self.backfill_command(event, 500).await,
            ["backfill", pages] => {
                let pages: u32 = pages
                    .parse()
                    .map_err(|_| CommandError::error(format!("Invalid max_pages: {pages}")))?;
                self.backfill_command(event, pages).await
            }
            ["quarantined-rooms", "reinstate", room_id] => {
                self.require_operator(event)?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm reinstating room {room_id}? Reply:\n!cumments quarantined-rooms reinstate {room_id} --confirm"
                    ),
                    site_id: None,
                })
            }
            ["quarantined-rooms", "reinstate", room_id, "--confirm"] => {
                self.require_operator(event)?;
                if !self.registry_store.reinstate_room(room_id).await? {
                    return Err(CommandError::error("Room not found in registry."));
                }
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Room {room_id} reinstated."),
                    site_id: None,
                })
            }
            ["rooms", "upgrades", "create", room_id, new_version] => {
                self.require_operator(event)?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm upgrading room {room_id} to {new_version}? Reply:\n!cumments rooms upgrades create {room_id} {new_version} --confirm"
                    ),
                    site_id: None,
                })
            }
            [
                "rooms",
                "upgrades",
                "create",
                room_id,
                new_version,
                "--confirm",
            ] => {
                self.require_operator(event)?;
                let driver = self.require_driver()?;
                let replacement = cumments_core::management::upgrade_comment_room(
                    driver,
                    self.registry_store.as_ref(),
                    self.site_service.as_ref(),
                    room_id,
                    new_version,
                )
                .await?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Room {room_id} upgraded to {new_version}, new room: {replacement}"
                    ),
                    site_id: None,
                })
            }
            ["rooms", "retirements", "create", room_id] => {
                self.require_operator(event)?;
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!(
                        "Confirm retiring room {room_id}? This cannot be undone. Reply:\n!cumments rooms retirements create {room_id} --confirm"
                    ),
                    site_id: None,
                })
            }
            ["rooms", "retirements", "create", room_id, "--confirm"] => {
                self.require_operator(event)?;
                if !cumments_core::management::retire_page_room_by_room_id(
                    self.registry_store.as_ref(),
                    room_id,
                )
                .await?
                {
                    return Err(CommandError::error("Room not found or not active."));
                }
                self.governance_notify.notify_one();
                Ok(CommandOutcome {
                    invalid: false,
                    reply: format!("Room {room_id} marked as retired. Processing in background."),
                    site_id: None,
                })
            }
            _ => Ok(CommandOutcome::invalid(help_text())),
        }
    }

    async fn backfill_command(
        &self,
        event: &ParsedRoomMessage,
        max_pages: u32,
    ) -> Result<CommandOutcome, CommandError> {
        self.require_operator(event)?;
        let Some(tx) = &self.backfill_tx else {
            return Ok(CommandOutcome::plain(
                "Backfill not enabled (no worker in this process); use CLI: cumments backfill",
            ));
        };
        match tx.try_send(BackfillRequest {
            actor_mxid: event.sender.clone(),
            reply_room_id: event.room_id.clone(),
            max_pages,
        }) {
            Ok(()) => Ok(CommandOutcome::plain(format!(
                "Backfill started (up to {max_pages} pages per room), you will be notified when it completes."
            ))),
            Err(mpsc::error::TrySendError::Full(_)) => Ok(CommandOutcome::plain(
                "A backfill is already running. Please try again later.",
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Ok(CommandOutcome::plain("Backfill worker stopped."))
            }
        }
    }

    fn require_operator(&self, event: &ParsedRoomMessage) -> Result<(), CommandError> {
        if self.operator_mxids.iter().any(|m| m == &event.sender) {
            Ok(())
        } else {
            Err(CommandError::denied(
                "This command is restricted to instance operators.",
            ))
        }
    }

    async fn require_site_access(
        &self,
        event: &ParsedRoomMessage,
        site_id: &str,
    ) -> Result<(), CommandError> {
        if self.operator_mxids.iter().any(|m| m == &event.sender) {
            return Ok(());
        }
        self.require_state_permission(event, site_id, POWER_LEVELS_EVENT_TYPE)
            .await?;
        Ok(())
    }

    /// Room-level governance check: the sender must be able to write
    /// `m.room.power_levels` in the comment room itself. With the room
    /// threshold pinned to 75 this matches the Matrix client exactly:
    /// managers and admins can appoint moderators, 50-level moderators
    /// cannot.
    async fn require_room_state_permission(
        &self,
        event: &ParsedRoomMessage,
        room_id: &str,
    ) -> Result<(), CommandError> {
        if self.operator_mxids.iter().any(|m| m == &event.sender) {
            return Ok(());
        }
        let driver = self.require_driver()?;
        let power_levels = driver
            .get_room_power_levels(room_id)
            .await
            .map_err(CommandError::error)?
            .unwrap_or_else(|| serde_json::json!({}));
        if can_send_state_event(&power_levels, &event.sender, POWER_LEVELS_EVENT_TYPE) {
            Ok(())
        } else {
            Err(CommandError::denied(format!(
                "You don't have permission to perform this operation in room {room_id}."
            )))
        }
    }

    /// Sticker-pack management follows the Matrix permission for writing
    /// `m.room.image_pack` state in the site Space (state_default by
    /// default), so managers are allowed exactly like in a Matrix client.
    async fn require_site_sticker_access(
        &self,
        event: &ParsedRoomMessage,
        site_id: &str,
    ) -> Result<(), CommandError> {
        if self.operator_mxids.iter().any(|m| m == &event.sender) {
            return Ok(());
        }
        self.require_state_permission(event, site_id, IMAGE_PACK_EVENT_TYPE)
            .await?;
        Ok(())
    }

    async fn require_state_permission(
        &self,
        event: &ParsedRoomMessage,
        site_id: &str,
        event_type: &str,
    ) -> Result<(), CommandError> {
        let site_id = SiteId::new(site_id.to_string()).map_err(CommandError::error)?;
        let Some(space_id) = self
            .site_service
            .space_id(&site_id)
            .await
            .map_err(CommandError::error)?
        else {
            return Err(CommandError::denied(format!(
                "Site {} does not have a Matrix Space yet.",
                site_id.as_str()
            )));
        };
        let driver = self.require_driver()?;
        let power_levels = driver
            .get_room_power_levels(&space_id)
            .await
            .map_err(CommandError::error)?
            .unwrap_or_else(|| serde_json::json!({}));
        if can_send_state_event(&power_levels, &event.sender, event_type) {
            Ok(())
        } else {
            Err(CommandError::denied(format!(
                "You don't have permission to perform this operation for site {}.",
                site_id.as_str()
            )))
        }
    }

    async fn active_site_for(&self, event: &ParsedRoomMessage) -> Result<String, CommandError> {
        let mut active_sites = self.active_sites.lock().await;
        if let Some(id) = active_sites.get(&event.sender).cloned() {
            return Ok(id);
        }
        drop(active_sites);
        // Fall back to the sites the sender administers: a single site is
        // automatic, multiple sites list the ambiguity instead of guessing.
        let mut owned = Vec::new();
        for site in self.site_auth_store.list_site_auth().await? {
            let roles = self.governance_store.list_site_roles(&site.site_id).await?;
            if roles
                .iter()
                .any(|role| role.level == SITE_ADMIN_LEVEL && role.user_id == event.sender)
            {
                owned.push(site.site_id);
            }
        }
        match owned.len() {
            0 => Err(CommandError::error(
                "You are not an admin of any site. Register first with `!cumments sites register <site_id>`.",
            )),
            1 => Ok(owned.remove(0)),
            _ => Err(CommandError::error(format!(
                "You manage multiple sites: {}. Use `!cumments sites use <site_id>` to select one.",
                owned.join(", ")
            ))),
        }
    }

    async fn site_status(&self, site_id: &str) -> Result<String, CommandError> {
        let Some(auth) = self.site_auth_store.get_site_auth(site_id).await? else {
            if let Some(entry) = self.site_auth_policy.entry(site_id) {
                return Ok(format!(
                    "Site {site_id}\nSource: config\nAuth: {}\nChat commands only manage API-registered sites.",
                    entry.auth_mode.unwrap_or(SiteAuthMode::Origin).as_str()
                ));
            }
            return Err(CommandError::error("Site not found."));
        };
        let roles = self.governance_store.list_site_roles(site_id).await?;
        let admins = roles
            .iter()
            .filter(|r| r.level == SITE_ADMIN_LEVEL)
            .map(|r| r.user_id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let managers = roles
            .iter()
            .filter(|r| r.level == MANAGER_LEVEL)
            .map(|r| r.user_id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "Site {site_id}\nStatus: {}\nSite admins: {}\nManagers: {}",
            auth.auth_mode.as_str(),
            if admins.is_empty() { "(none)" } else { &admins },
            if managers.is_empty() {
                "(none)"
            } else {
                &managers
            },
        ))
    }

    fn require_driver(&self) -> Result<&dyn cumments_core::ports::MatrixDriver, CommandError> {
        self.driver
            .as_deref()
            .ok_or_else(|| CommandError::error("No Matrix driver in current mode."))
    }
}

impl EventProcessor {
    pub fn new(deps: EventProcessorDeps) -> Self {
        let command_router = BotCommandRouter::new(&deps);
        Self {
            site_store: deps.site_store,
            registry_store: deps.registry_store,
            message_store: deps.message_store,
            room_store: deps.room_store,
            governance_store: deps.governance_store,
            sticker_pack_store: deps.sticker_pack_store,
            projection_repair_store: deps.projection_repair_store,
            role_claim_store: deps.role_claim_store,
            _submission_store: deps.submission_store,
            site_service: deps.site_service,
            driver: deps.driver,
            event_bus: deps.event_bus,
            projection_notify: deps.projection_notify,
            server_name: deps.server_name,
            invite_join_limiter: SlidingWindowRateLimiter::new(
                5,
                StdDuration::from_secs(60),
                cumments_core::rate_limit::DEFAULT_MAX_KEYS,
            ),
            command_router,
            event_capture: Mutex::new(None),
        }
    }

    pub async fn start_event_capture(&self) {
        let mut capture = self.event_capture.lock().await;
        *capture = Some(Vec::new());
    }

    /// Returns captured events, or `None` when direct broadcast is enabled.
    pub async fn stop_event_capture(&self) -> Option<Vec<ProjectorEvent>> {
        self.event_capture.lock().await.take()
    }

    async fn emit(&self, event: ProjectorEvent) {
        if let Some(capture) = self.event_capture.lock().await.as_mut() {
            capture.push(event);
            return;
        }
        let _ = self.event_bus.send(event);
    }

    /// Look up the site ID associated with a Matrix Space room ID.
    /// Returns `None` if the space is not in our local database.
    pub async fn get_site_id_by_space_id(&self, space_id: &str) -> Result<Option<String>> {
        self.site_store
            .get_site_by_space_id(space_id)
            .await
            .map(|site| site.map(|s| s.id))
    }

    /// Attempts to match a plain-text message against a pending token-DM role
    /// claim. Returns `true` when the message activated a claim and therefore
    /// must not be projected as a comment.
    pub async fn process_claim_dm(&self, event: &ParsedRoomMessage) -> Result<bool> {
        if event.is_virtual_user_sender {
            return Ok(false);
        }
        let Content::Text(text) = &event.content else {
            return Ok(false);
        };
        if text.style != TextStyle::Normal
            || event.relates_to.is_some()
            || event.reply_to.is_some()
            || event.thread_root.is_some()
        {
            return Ok(false);
        }
        let Some(token) = text.body.trim().strip_prefix(CLAIM_MESSAGE_PREFIX) else {
            return Ok(false);
        };
        let token = token.trim();
        if token.is_empty() {
            return Ok(false);
        }

        // Claim tokens are capabilities: activate them only in a verified
        // private channel (exactly the bot and the sender). Fail closed when
        // the channel cannot be verified.
        if !is_private_channel(event, &self.driver).await? {
            warn!(
                "Rejecting claim from {} in {}: not a verified private channel",
                event.sender, event.room_id,
            );
            return Ok(false);
        }

        let presented_hash = sha256_hex(token.as_bytes());
        for claim in self
            .role_claim_store
            .pending_claims_for_user(&event.sender)
            .await?
        {
            if constant_time_eq(claim.token_hash.as_bytes(), presented_hash.as_bytes())
                && self.role_claim_store.mark_claim_activated(claim.id).await?
            {
                info!("Activated role claim {} for {}", claim.id, claim.user_id);
                self.projection_notify.notify_one();
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Handles `!cumments ...` messages. Returns `true` when the message was
    /// consumed as a command (and must not be projected as a comment).
    pub async fn process_bot_command(&self, event: &ParsedRoomMessage) -> Result<bool> {
        self.command_router.process_bot_command(event).await
    }

    /// Resolve the Cumments identity of a room from the local registry.
    ///
    /// This is the AppService-mode counterpart of reading room state metadata:
    /// the reconciler writes the room mapping back to the registry when it
    /// creates or adopts a room, so incoming push events can be attributed
    /// without any extra homeserver API call.
    pub async fn resolve_room_identity(&self, room_id: &str) -> Result<Option<RoomIdentity>> {
        self.registry_store
            .get_registered_room_identity(room_id)
            .await
    }

    /// Resolves the author profile snapshot at projection time.
    ///
    /// Both Matrix-native and virtual-user senders take their display name
    /// and avatar from the current `m.room.member` state: profile data is
    /// Matrix state, never signed event content. The stored value is a
    /// fallback; the API/SSE read path joins live member state on output
    /// (see `misc/design/visitor-identity.md`).
    fn author_profile_snapshot(member: Option<&RoomMember>) -> (Option<String>, Option<String>) {
        (
            member.and_then(|member| member.display_name.clone()),
            member.and_then(|member| member.avatar_url.clone()),
        )
    }

    /// Process a room message (new comment or edit).
    #[instrument(skip(self))]
    pub async fn process_room_message(&self, event: ParsedRoomMessage) -> Result<()> {
        // ── PRINCIPLE B: REGISTRY ENFORCEMENT ──
        let registry_status = self.registry_store.get_room_status(&event.room_id).await?;

        match registry_status {
            Some(RoomStatus::Active) => {
                // Room is active, proceed normally
            }
            Some(_) => {
                // Room is quarantined, superseded or otherwise not canonical.
                debug!("Ignoring message from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                // Push events carry no room state, so an unknown room has no
                // identity to register from; rooms are registered ahead of
                // event processing by the reconciler, space-child discovery,
                // or backfill.
                debug!("Ignoring message from unregistered room {}", event.room_id);
                return Ok(());
            }
        }

        // 0. Identify the room context
        let (site_id, page_slug) = match event.room_identity {
            Some(ref id)
                if SiteId::new(id.site_id.clone()).is_ok()
                    && PageSlug::new(id.page_slug.clone()).is_ok() =>
            {
                (id.site_id.clone(), id.page_slug.clone())
            }
            Some(ref id) => {
                warn!(
                    "Ignoring message from room {} with invalid identity {}/{}",
                    event.room_id, id.site_id, id.page_slug
                );
                return Ok(());
            }
            None => return Ok(()), // Not a cumments room
        };

        // B-04: a redaction may have been seen before its target (capped or
        // resumed backfill, push retry). Never re-project a tombstoned event.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned event {}", event.event_id);
            return Ok(());
        }

        // Handle Edits (Replacements)
        if let Some(ref relation) = event.relates_to {
            info!("Handling edit for event {}", relation.target_event_id);

            let existing = self
                .message_store
                .get_message(&relation.target_event_id)
                .await?;

            // A redacted original is a terminal tombstone. This check also
            // prevents an edit that raced the redaction from restoring content.
            if existing
                .as_ref()
                .is_some_and(|message| message.status == MessageStatus::Redacted)
            {
                debug!(
                    "Ignoring edit for {}: target is redacted",
                    relation.target_event_id
                );
                return Ok(());
            }

            // Replacement validity: same Matrix event type and same sender.
            // Legacy rows without a recorded sender are accepted until
            // re-projected by backfill.
            if let Some(existing) = existing.as_ref() {
                if existing.matrix_event_type != event.event_type {
                    debug!(
                        "Ignoring edit for {}: replacement type {} does not match {}",
                        relation.target_event_id, event.event_type, existing.matrix_event_type
                    );
                    return Ok(());
                }
                if !existing.sender_mxid.is_empty() && existing.sender_mxid != event.sender {
                    warn!(
                        "Rejecting edit for {} from {}: sender does not match original author {}",
                        relation.target_event_id, event.sender, existing.sender_mxid
                    );
                    return Ok(());
                }
            }

            // Visitor edits must carry a valid Cumments identity block and
            // signature; Matrix-native edits are governed by the sender check
            // above.
            if event.is_virtual_user_sender {
                let valid = match (
                    &event.author_public_key,
                    &event.author_signature,
                    &event.author_challenge,
                ) {
                    (Some(pk), Some(sig), Some(chal)) => {
                        relation.signable_content().is_some_and(|new_content| {
                            let message = signature_message(&[
                                Some("PATCH"),
                                Some(site_id.as_str()),
                                Some(page_slug.as_str()),
                                Some(relation.target_event_id.as_str()),
                                Some(new_content),
                                Some(chal),
                                Some("1"),
                            ]);
                            verify_visitor_event(
                                self.server_name.as_deref(),
                                &event.sender,
                                &site_id,
                                pk,
                                sig,
                                &message,
                            )
                        })
                    }
                    _ => false,
                };
                if !valid {
                    warn!(
                        "Rejecting visitor edit for {} from {}: missing or invalid Cumments identity block",
                        relation.target_event_id, event.sender
                    );
                    return Ok(());
                }
            }

            let edited_at = chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
            let Some(mut updated) = existing else {
                debug!(
                    "Edit ignored for {}: target missing",
                    relation.target_event_id
                );
                return Ok(());
            };
            updated.content = relation.new_content.clone();
            updated.edited_at = Some(edited_at);
            let revision = MessageRevision {
                event_id: event.event_id.clone(),
                message_event_id: updated.event_id.clone(),
                content: updated.content.clone(),
                edited_at,
                editor_mxid: event.sender.clone(),
                redacted_at: None,
            };

            // The projection and queue closure share one SQLite transaction.
            let completion = if let Some(id) = event.submission_id {
                SubmissionCompletion::UpdateById(id)
            } else {
                SubmissionCompletion::UpdateByEvent {
                    target_event_id: relation.target_event_id.clone(),
                    author_public_key: event.author_public_key.clone(),
                }
            };
            let outcome = self
                .message_store
                .apply_edit_unit(&updated, &revision, completion)
                .await?;
            let should_complete = match outcome {
                EditProjectionOutcome::AppliedCurrent | EditProjectionOutcome::AlreadyKnown => true,
                // A correlated stale command was still observed on the
                // homeserver; an uncorrelated stale event must not close a
                // different queued edit.
                EditProjectionOutcome::Superseded => event.submission_id.is_some(),
                EditProjectionOutcome::Rejected => false,
            };

            if should_complete {
                info!("Successfully updated message {}", relation.target_event_id);
                if let Some(message) = self
                    .message_store
                    .get_message(&relation.target_event_id)
                    .await?
                {
                    self.emit(ProjectorEvent::MessageUpdated {
                        site_id,
                        page_slug,
                        message,
                    })
                    .await;
                }
            } else {
                debug!(
                    ?outcome,
                    "Edit not eligible for closure: {}", relation.target_event_id
                );
            }
            return Ok(());
        }

        // Visitor posts must carry a valid Cumments identity block and
        // signature. Matrix-native posts skip this path entirely: their
        // identity is the Matrix sender itself.
        if event.is_virtual_user_sender {
            let valid = match (
                &event.author_public_key,
                &event.author_signature,
                &event.author_challenge,
            ) {
                (Some(pk), Some(sig), Some(chal)) => {
                    let message = match &event.content {
                        // Locations use the standalone LOCATE signature;
                        // text/media use the POST signature (which binds the
                        // content and reply relation).
                        Content::Location(location) => signature_message(&[
                            Some("LOCATE"),
                            Some(site_id.as_str()),
                            Some(page_slug.as_str()),
                            Some(location.geo_uri.as_str()),
                            event.reply_to.as_deref(),
                            event.thread_root.as_deref(),
                            Some(chal),
                            Some("1"),
                        ]),
                        _ => match event.signable_content() {
                            Some(content) => post_signature_message(
                                &site_id,
                                &page_slug,
                                content,
                                event.reply_to.as_deref(),
                                event.thread_root.as_deref(),
                                chal,
                            ),
                            None => return Ok(()),
                        },
                    };
                    verify_visitor_event(
                        self.server_name.as_deref(),
                        &event.sender,
                        &site_id,
                        pk,
                        sig,
                        &message,
                    )
                }
                _ => false,
            };
            if !valid {
                warn!(
                    "Rejecting visitor post {} from {}: missing or invalid Cumments identity block",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }

        // Handle original messages.
        let is_matrix_native = !event.is_virtual_user_sender;
        let member = self
            .room_store
            .get_member(&event.room_id, &event.sender)
            .await?;
        let (display_name, avatar_url) = Self::author_profile_snapshot(member.as_ref());
        let message = Message {
            event_id: event.event_id.clone(),
            site_id: site_id.clone(),
            page_slug: page_slug.clone(),
            author: AuthorSnapshot {
                kind: if is_matrix_native {
                    AuthorKind::Matrix
                } else {
                    AuthorKind::Visitor
                },
                display_name,
                avatar_url,
                public_key: event.author_public_key.clone(),
                mxid: if is_matrix_native {
                    Some(event.sender.clone())
                } else {
                    None
                },
            },
            content: event.content.clone(),
            matrix_event_type: event.event_type.clone(),
            timestamp: chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
            edited_at: None,
            reply_to: event.reply_to.clone(),
            thread_root: event.thread_root.clone(),
            submission_id: event.submission_id,
            status: MessageStatus::Active,
            redacted_at: None,
            redacted_by: None,
            reactions: Vec::new(),
            thread_summary: None,
            room_id: event.room_id.clone(),
            sender_mxid: event.sender.clone(),
            raw_content: event.raw_content.clone(),
        };

        // The fact and its closed-loop completion commit together; SSE is sent
        // only after this method returns.
        let completion = match event.submission_id {
            Some(id) => SubmissionCompletion::PostById(id),
            None => SubmissionCompletion::PostByEvent(event.event_id.clone()),
        };
        let outcome = self
            .message_store
            .save_message_unit(&message, completion)
            .await?;
        info!(
            ?outcome,
            event_id = %event.event_id,
            "Observed projected message event"
        );
        self.emit(ProjectorEvent::MessageCreated {
            site_id,
            page_slug,
            message,
        })
        .await;
        Ok(())
    }

    /// Process a reaction event (m.reaction) into the annotations store.
    #[instrument(skip(self))]
    pub async fn process_reaction(&self, event: ParsedReaction) -> Result<()> {
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring reaction from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!("Ignoring reaction from unregistered room {}", event.room_id);
                return Ok(());
            }
        }
        // A redaction may have been seen before its target (capped/resumed
        // backfill, push retry); never re-project a tombstoned reaction.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned reaction {}", event.event_id);
            return Ok(());
        }
        // Visitor reactions carry a signed proof block that must verify.
        if event.is_virtual_user_sender {
            let (Some(pk), Some(sig), Some(chal)) = (
                event.author_public_key.as_deref(),
                event.author_signature.as_deref(),
                event.author_challenge.as_deref(),
            ) else {
                warn!(
                    "Rejecting visitor reaction {} from {}: missing proof block",
                    event.event_id, event.sender
                );
                return Ok(());
            };
            let Some(identity) = &event.room_identity else {
                debug!("Ignoring reaction {} without room identity", event.event_id);
                return Ok(());
            };
            let message = signature_message(&[
                Some("REACT"),
                Some(identity.site_id.as_str()),
                Some(identity.page_slug.as_str()),
                Some(event.message_event_id.as_str()),
                Some(event.key.as_str()),
                Some(chal),
                Some("1"),
            ]);
            if !verify_visitor_event(
                self.server_name.as_deref(),
                &event.sender,
                &identity.site_id,
                pk,
                sig,
                &message,
            ) {
                warn!(
                    "Rejecting visitor reaction {} from {}: invalid proof",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }
        // The reaction must target a projected message in the same room.
        let Some(target) = self
            .message_store
            .get_message(&event.message_event_id)
            .await?
        else {
            debug!(
                "Ignoring reaction {} for unknown message {}",
                event.event_id, event.message_event_id
            );
            return Ok(());
        };
        if target.room_id != event.room_id {
            warn!(
                "Ignoring reaction {} for {}: message lives in {}",
                event.event_id, event.message_event_id, target.room_id
            );
            return Ok(());
        }
        let message_event_id = event.message_event_id.clone();
        self.message_store
            .save_reaction(&Reaction {
                event_id: event.event_id,
                message_event_id: message_event_id.clone(),
                sender_mxid: event.sender,
                key: event.key,
                origin_server_ts: event.origin_server_ts,
                redacted_at: None,
            })
            .await?;
        if let Some(updated) = self.message_store.get_message(&message_event_id).await? {
            self.emit(ProjectorEvent::MessageAnnotationsChanged {
                site_id: updated.site_id.clone(),
                page_slug: updated.page_slug.clone(),
                message: updated,
            })
            .await;
        }
        Ok(())
    }

    /// Process a poll response by mapping answer IDs to option indexes on the
    /// stored poll, then recording the vote.
    #[instrument(skip(self))]
    pub async fn process_poll_vote(&self, event: ParsedPollVote) -> Result<()> {
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring poll vote from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!(
                    "Ignoring poll vote from unregistered room {}",
                    event.room_id
                );
                return Ok(());
            }
        }
        // Same tombstone gate as messages/reactions: a redaction seen before
        // the original vote must prevent resurrection on re-delivery.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!("Ignoring tombstoned poll vote {}", event.event_id);
            return Ok(());
        }

        if event.is_virtual_user_sender {
            if event.answer_ids.len() != 1 {
                warn!(
                    "Rejecting visitor vote {} from {}: visitor votes are single-select",
                    event.event_id, event.sender
                );
                return Ok(());
            }
            let answer_id = event.answer_ids.first();
            let (Some(pk), Some(sig), Some(chal)) = (
                event.author_public_key.as_deref(),
                event.author_signature.as_deref(),
                event.author_challenge.as_deref(),
            ) else {
                warn!(
                    "Rejecting visitor vote {} from {}: missing proof block",
                    event.event_id, event.sender
                );
                return Ok(());
            };
            let Some(identity) = &event.room_identity else {
                debug!(
                    "Ignoring poll vote {} without room identity",
                    event.event_id
                );
                return Ok(());
            };
            let message = signature_message(&[
                Some("VOTE"),
                Some(identity.site_id.as_str()),
                Some(identity.page_slug.as_str()),
                Some(event.poll_message_id.as_str()),
                answer_id.map(String::as_str),
                Some(chal),
                Some("1"),
            ]);
            if !verify_visitor_event(
                self.server_name.as_deref(),
                &event.sender,
                &identity.site_id,
                pk,
                sig,
                &message,
            ) {
                warn!(
                    "Rejecting visitor vote {} from {}: invalid proof",
                    event.event_id, event.sender
                );
                return Ok(());
            }
        }

        let Some(message) = self
            .message_store
            .get_message(&event.poll_message_id)
            .await?
        else {
            debug!(
                "Poll vote for unknown poll {}; ignoring",
                event.poll_message_id
            );
            return Ok(());
        };
        let Content::Poll(poll) = &message.content else {
            debug!(
                "Poll vote target {} is not a poll; ignoring",
                event.poll_message_id
            );
            return Ok(());
        };
        if event
            .answer_ids
            .iter()
            .any(|answer_id| !poll.options.iter().any(|option| &option.id == answer_id))
        {
            let vote = PollVote {
                event_id: event.event_id.clone(),
                poll_message_id: event.poll_message_id.clone(),
                sender_mxid: event.sender.clone(),
                option_index: None,
                origin_server_ts: event.origin_server_ts,
            };
            self.message_store
                .save_poll_vote_with_selections(&vote, &[], Some("unknown_answer"))
                .await?;
        } else {
            // MSC3381 requires truncation to the declared limit; duplicates
            // remaining after truncation contribute only one selection.
            let mut selections = Vec::with_capacity(event.answer_ids.len());
            for answer_id in event.answer_ids.iter().take(poll.max_selections as usize) {
                if !selections.contains(answer_id) {
                    selections.push(answer_id.clone());
                }
            }
            let option_index = selections.first().and_then(|answer_id| {
                poll.options
                    .iter()
                    .position(|option| &option.id == answer_id)
                    .map(|index| index as i64)
            });
            self.message_store
                .save_poll_vote_with_selections(
                    &PollVote {
                        event_id: event.event_id,
                        poll_message_id: event.poll_message_id,
                        sender_mxid: event.sender,
                        option_index,
                        origin_server_ts: event.origin_server_ts,
                    },
                    &selections,
                    None,
                )
                .await?;
        }
        if let Some(updated) = self.message_store.get_message(&message.event_id).await? {
            self.emit(ProjectorEvent::MessageAnnotationsChanged {
                site_id: updated.site_id.clone(),
                page_slug: updated.page_slug.clone(),
                message: updated,
            })
            .await;
        }
        Ok(())
    }

    /// Process a room state event (system message / room metadata).
    #[instrument(skip(self))]
    pub async fn process_room_state(&self, event: ParsedRoomState) -> Result<()> {
        if event.event_type == "m.room.member" {
            let membership = event
                .content
                .get("membership")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Conditional auto-join: accept claim-DM invites only when the
            // Invite admission (membership/resource, pre-join) vs governance
            // authorization (post-join private-channel + command auth) are
            // independent. Bot membership does not grant governance authority,
            // but it does expand the AS-visible event stream. The invite-side
            // SlidingWindowRateLimiter(5/min per inviter) bounds join-admission
            // attempts, not successful joins, before pending-claim lookup.
            if membership == "invite"
                && self
                    .driver
                    .as_ref()
                    .and_then(|driver| driver.sender_user_id())
                    .as_deref()
                    == Some(event.state_key.as_str())
            {
                let inviter = &event.sender;
                // Per-inviter resource bound for all Bot join-admission attempts
                // (claim + bootstrap). Denied attempts do not query pending
                // claims or call join_room; admitted attempts consume quota
                // before classification, even if bootstrap validation or
                // join_room later fails.
                if !self.invite_join_limiter.allow(inviter) {
                    debug!(
                        "Ignoring invite for {} in {}: join_rate_limited",
                        inviter, event.room_id
                    );
                } else {
                    let claims = self
                        .role_claim_store
                        .pending_claims_for_user(inviter)
                        .await?;
                    if !claims.is_empty() {
                        // Existing capability-based path — preserve semantics,
                        // including federated claimants. Do not narrow by
                        // locality or extra identity filtering here.
                        if let Some(driver) = &self.driver {
                            match driver.join_room(&event.room_id).await {
                                Ok(()) => {
                                    self.role_claim_store
                                        .set_claim_dm_room_for_user(inviter, &event.room_id)
                                        .await?;
                                    info!(
                                        "Bot joined claim DM {} for {} (reason=role_claim)",
                                        event.room_id, inviter
                                    );
                                }
                                Err(e) => warn!(
                                    "Bot failed to join claim DM {} for {}: {:#}",
                                    event.room_id, inviter, e
                                ),
                            }
                        }
                    } else {
                        // Self-service bootstrap — only when no pending claim.
                        // Requires a normal Matrix governance identity (valid
                        // MXID, not AS-managed). No local-server restriction;
                        // federated users are first-class.
                        match validate_governance_user_id(inviter) {
                            Ok(_) => {
                                if let Some(driver) = &self.driver {
                                    match driver.join_room(&event.room_id).await {
                                        Ok(()) => {
                                            info!(
                                                "Bot joined bootstrap DM {} for {} (reason=self_service_bootstrap)",
                                                event.room_id, inviter
                                            );
                                        }
                                        Err(e) => warn!(
                                            "Bot failed to join bootstrap DM {} for {}: {:#}",
                                            event.room_id, inviter, e
                                        ),
                                    }
                                }
                            }
                            Err(
                                cumments_core::governance::GovernanceUserIdError::ServiceAccount,
                            ) => {
                                debug!(
                                    "Ignoring invite for {} in {}: managed_user",
                                    inviter, event.room_id
                                );
                            }
                            Err(_) => {
                                debug!(
                                    "Ignoring invite for {} in {}: invalid_user",
                                    inviter, event.room_id
                                );
                            }
                        }
                    }
                }
            }
            let display_name = event
                .content
                .get("displayname")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let avatar_url = event
                .content
                .get("avatar_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // Leave events usually omit the profile; keep the last known one
            // instead of wiping it from the member snapshot.
            let (display_name, avatar_url) = if membership == "leave" {
                let existing = self
                    .room_store
                    .get_member(&event.room_id, &event.state_key)
                    .await?;
                (
                    display_name.or_else(|| existing.as_ref().and_then(|m| m.display_name.clone())),
                    avatar_url.or_else(|| existing.as_ref().and_then(|m| m.avatar_url.clone())),
                )
            } else {
                (display_name, avatar_url)
            };
            self.room_store
                .save_member(&RoomMember {
                    room_id: event.room_id.clone(),
                    // `m.room.member` state key is the member's user ID.
                    user_id: event.state_key.clone(),
                    display_name,
                    avatar_url,
                    membership,
                    updated_at: chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
                        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                })
                .await?;
        }

        // A native tombstone is trusted only when the AS sender issued it.
        // Cumments deliberately locks `m.room.tombstone` to the bot, so a
        // non-bot sender means the governance invariant was bypassed. Never
        // auto-adopt that replacement: quarantine the old active mapping for
        // manual review instead of letting it hijack a site/page identity.
        if event.event_type == "m.room.tombstone" {
            let Some(replacement) = event
                .content
                .get("replacement_room")
                .and_then(|v| v.as_str())
            else {
                return Ok(());
            };
            let intent = self
                .registry_store
                .get_upgrade_intent(&event.room_id)
                .await?;
            if matches!(
                self.registry_store.get_room_status(&event.room_id).await?,
                Some(RoomStatus::Active)
            ) {
                let as_sender = self
                    .driver
                    .as_ref()
                    .and_then(|driver| driver.sender_user_id());
                let authorized_intent = intent.as_ref().is_some_and(|intent| {
                    matches!(
                        intent.status,
                        cumments_core::models::RoomUpgradeIntentStatus::Requested
                            | cumments_core::models::RoomUpgradeIntentStatus::Observed
                            | cumments_core::models::RoomUpgradeIntentStatus::Adopted
                    ) && intent
                        .replacement_room_id
                        .as_deref()
                        .is_none_or(|observed| observed == replacement)
                });
                if event.sender != as_sender.unwrap_or_default() || !authorized_intent {
                    self.registry_store
                        .mark_upgrade_intent_manual(
                            &event.room_id,
                            &format!(
                                "unmanaged native room upgrade by {} to {replacement}",
                                event.sender
                            ),
                        )
                        .await?;
                    self.registry_store
                        .quarantine_room(
                            &event.room_id,
                            &format!(
                                "unexpected native room upgrade by {}; manual successor review required",
                                event.sender
                            ),
                            1,
                            None,
                        )
                        .await?;
                    warn!(
                        old_room = %event.room_id,
                        new_room = %replacement,
                        sender = %event.sender,
                        "Unmanaged native room upgrade quarantined; refusing automatic \
                         adoption"
                    );
                    return Ok(());
                }

                let Some(intent) = intent else {
                    return Ok(());
                };
                if intent.status == cumments_core::models::RoomUpgradeIntentStatus::Adopted {
                    return Ok(());
                }

                self.registry_store
                    .observe_upgrade_replacement(&event.room_id, replacement)
                    .await?;
                let Some(driver) = self.driver.as_deref() else {
                    self.registry_store
                        .mark_upgrade_intent_manual(
                            &event.room_id,
                            "AS driver unavailable for managed upgrade reconciliation",
                        )
                        .await?;
                    return Ok(());
                };
                match cumments_core::management::upgrade_comment_room(
                    driver,
                    self.registry_store.as_ref(),
                    &self.site_service,
                    &event.room_id,
                    &intent.expected_new_version,
                )
                .await
                {
                    Ok(observed_replacement) if observed_replacement == replacement => {}
                    Ok(observed_replacement) => {
                        self.registry_store
                            .mark_upgrade_intent_manual(
                                &event.room_id,
                                &format!(
                                    "tombstone replacement {replacement} conflicts with recovered {observed_replacement}"
                                ),
                            )
                            .await?;
                    }
                    Err(e) => {
                        warn!(
                            old_room = %event.room_id,
                            new_room = %replacement,
                            error = %e,
                            "Managed native room upgrade reconciliation failed"
                        );
                    }
                }
                return Ok(());
            }

            // Replays after successful convergence arrive against an inactive
            // old room. Closing the intent here keeps repeated tombstones
            // idempotent without resurrecting the old registry mapping.
            if let Some(intent) = intent
                && intent.replacement_room_id.as_deref() == Some(replacement)
            {
                self.registry_store
                    .complete_upgrade_intent(&event.room_id, replacement)
                    .await?;
            }
        }

        if event.event_type == "m.room.encryption"
            && self
                .role_claim_store
                .claim_dm_room_exists(&event.room_id)
                .await?
        {
            warn!(
                "Claim DM {} is encrypted; verification tokens cannot be read \
                 (claim DMs must be unencrypted)",
                event.room_id
            );
        }
        if event.event_type == POWER_LEVELS_EVENT_TYPE {
            let site = self.site_store.get_site_by_space_id(&event.room_id).await?;
            // A site Space's power levels define site roles (>= manager);
            // comment rooms additionally carry per-room moderators (>= 50).
            let min_level = if site.is_some() {
                SITE_ROLE_MIN_LEVEL
            } else {
                MODERATOR_LEVEL
            };
            let roles: Vec<RoleEntry> = role_entries(&event.content, min_level)
                .into_iter()
                .filter(|role| !is_as_managed_user(&role.user_id))
                .collect();
            if let Some(site) = site {
                self.governance_store
                    .replace_site_roles(&site.id, &roles)
                    .await?;
                self.projection_notify.notify_one();
            } else if matches!(
                self.registry_store.get_room_status(&event.room_id).await?,
                Some(RoomStatus::Active)
            ) {
                self.governance_store
                    .replace_room_roles(&event.room_id, &roles)
                    .await?;
            }
        }

        if event.event_type == IMAGE_PACK_EVENT_TYPE {
            self.project_image_pack(&event).await?;
        }

        self.room_store
            .save_state_event(&RoomStateEvent {
                event_id: event.event_id,
                room_id: event.room_id,
                event_type: event.event_type,
                state_key: event.state_key,
                sender: event.sender,
                origin_server_ts: event.origin_server_ts,
                content_json: event.content,
            })
            .await?;
        Ok(())
    }

    /// Projects one `m.room.image_pack` state event into the sticker-pack
    /// read model. Packs live on site Spaces only; anything else is ignored.
    ///
    /// A well-formed pack whose `usage` no longer includes stickers, or a
    /// malformed pack, replaces (removes) any previous projection for the
    /// same site + state key: the current Matrix state is authoritative.
    async fn project_image_pack(&self, event: &ParsedRoomState) -> Result<()> {
        // A redaction was already observed for this event (push retry or a
        // resumed backfill replay); never resurrect the pack.
        if self
            .message_store
            .has_backfill_tombstone(&event.event_id, &event.room_id)
            .await?
        {
            debug!(
                "Ignoring tombstoned image pack event {} in {}",
                event.event_id, event.room_id
            );
            return Ok(());
        }

        let Some(site) = self.site_store.get_site_by_space_id(&event.room_id).await? else {
            debug!("Ignoring image pack in non-space room {}", event.room_id);
            return Ok(());
        };

        match parse_image_pack_content(&event.room_id, &site.id, &event.state_key, &event.content) {
            Ok(Some(pack)) => {
                self.sticker_pack_store
                    .save_site_pack(&StickerPackProjection {
                        pack,
                        event_id: event.event_id.clone(),
                        sender: event.sender.clone(),
                        origin_server_ts: event.origin_server_ts,
                    })
                    .await?;
                info!(
                    "Projected sticker pack {}/{} from {}",
                    site.id, event.state_key, event.event_id
                );
            }
            Ok(None) => {
                warn!(
                    "Image pack {}/{} no longer targets stickers; removing projection",
                    site.id, event.state_key
                );
                self.sticker_pack_store
                    .delete_site_pack(&site.id, &event.state_key)
                    .await?;
            }
            Err(error) => {
                warn!(
                    "Dropping malformed image pack {}/{}: {error}",
                    site.id, event.state_key
                );
                self.sticker_pack_store
                    .delete_site_pack(&site.id, &event.state_key)
                    .await?;
            }
        }
        Ok(())
    }

    /// Process a redaction event (comment deletion).
    #[instrument(skip(self))]
    pub async fn process_room_redaction(&self, event: ParsedRoomRedaction) -> Result<()> {
        let target_event_id = match event.redacts {
            Some(ref id) => id.clone(),
            None => {
                debug!("Redaction event without a target event_id, ignoring");
                return Ok(());
            }
        };

        // 0. Sticker-pack state targets live on site Spaces, which are not in
        // the comment-room registry, so they are handled before the registry
        // gate below. Redacting the *current* pack event removes the pack
        // (redacted state keeps its slot with empty content per the spec).
        if let Some((site_id, state_key)) = self
            .sticker_pack_store
            .find_pack_by_event_id(&target_event_id)
            .await?
        {
            let Some(pack) = self
                .sticker_pack_store
                .get_site_pack(&site_id, &state_key)
                .await?
            else {
                return Ok(());
            };
            if pack.pack.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: pack lives in {}",
                    target_event_id, event.room_id, pack.pack.room_id
                );
                return Ok(());
            }
            self.sticker_pack_store
                .delete_site_pack(&site_id, &state_key)
                .await?;
            self.message_store
                .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                .await?;
            info!("Successfully redacted sticker pack {site_id}/{state_key} ({target_event_id})");
            return Ok(());
        }

        // 0b. Generic state-event targets (name, topic, avatar, power levels,
        // membership, ...). Redaction strips the event's content per the
        // room-version algorithm while keeping the state slot; the raw row is
        // updated in place so live pushes and backfill replay agree. Derived
        // projections are recomputed when the redacted event is the latest
        // version of its slot.
        if let Some(state) = self.room_store.get_state_event(&target_event_id).await? {
            if state.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: state event lives in {}",
                    target_event_id, event.room_id, state.room_id
                );
                return Ok(());
            }
            let resolved_snapshot = self
                .room_store
                .get_room_state_snapshot(&event.room_id)
                .await?
                .map(|snapshot| snapshot.room_version);
            let room_version = match resolved_snapshot {
                // The reconciler's homeserver-resolved state is authoritative.
                Some(room_version) => room_version,
                None => self
                    .room_store
                    .get_latest_state_event(&event.room_id, "m.room.create", "")
                    .await?
                    .and_then(|create| {
                        create
                            .content_json
                            .get("room_version")
                            .and_then(|version| version.as_str())
                            .map(str::to_string)
                    }),
            };
            let stripped = match redact_state_content_for_version(
                &state.event_type,
                &state.content_json,
                room_version.as_deref(),
            ) {
                Ok(stripped) => stripped,
                Err(UnsupportedRoomVersion(version)) => {
                    let error = format!(
                        "cannot apply state redaction with unknown room version {version:?}"
                    );
                    error!(
                        room_id = %event.room_id,
                        version = ?version,
                        "Refusing state redaction with unknown room version"
                    );
                    // Do not guess an unknown room version's protected keys.
                    // Queue identifiers for authoritative repair and ACK the
                    // push so this poison event cannot block its transaction.
                    self.projection_repair_store
                        .record_projection_repair(&ProjectionRepairInput {
                            target_event_id: target_event_id.clone(),
                            room_id: event.room_id.clone(),
                            redaction_event_id: event.event_id.clone(),
                            reason: "unsupported_room_version",
                            observed_room_version: version,
                            error,
                        })
                        .await?;
                    warn!(
                        room_id = %event.room_id,
                        target = %target_event_id,
                        "Queued unsupported-version state redaction for repair"
                    );
                    return Ok(());
                }
            };
            if let Err(error) = self
                .apply_stripped_state_redaction(
                    &state,
                    stripped,
                    &event.event_id,
                    event.origin_server_ts,
                )
                .await
            {
                error!(
                    room_id = %event.room_id,
                    target = %target_event_id,
                    error = %error,
                    "Failed to apply supported state redaction"
                );
                return Err(error);
            }
            info!(
                "Successfully redacted state event {target_event_id} ({} in {})",
                state.event_type, state.room_id
            );
            return Ok(());
        }

        // Same registry gate as message processing: ignore redactions from
        // deactivated or unregistered rooms so tombstoned rooms cannot keep
        // mutating the read model through live pushes.
        match self.registry_store.get_room_status(&event.room_id).await? {
            Some(RoomStatus::Active) => {}
            Some(_) => {
                debug!("Ignoring redaction from non-active room {}", event.room_id);
                return Ok(());
            }
            None => {
                debug!(
                    "Ignoring redaction from unregistered room {}",
                    event.room_id
                );
                return Ok(());
            }
        }

        // Replacement revisions are relation facts with their own redaction
        // state. Redacting one rolls the parent back to the latest surviving
        // edit or to its original content.
        let redacted_at = chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        let redacted_by = event
            .sender
            .clone()
            .unwrap_or_else(|| event.event_id.clone());
        if let Some(revision) = self
            .message_store
            .get_message_revision(&target_event_id)
            .await?
        {
            let Some(parent) = self
                .message_store
                .get_message(&revision.message_event_id)
                .await?
            else {
                debug!(
                    "Redaction tombstoned for revision {}: parent unknown",
                    target_event_id
                );
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                return Ok(());
            };
            if parent.room_id != event.room_id {
                warn!(
                    "Ignoring revision redaction {} in {}: edit lives in {}",
                    target_event_id, event.room_id, parent.room_id
                );
                return Ok(());
            }
            if self
                .message_store
                .redact_message_revision(
                    &target_event_id,
                    &event.room_id,
                    redacted_at,
                    redacted_by.as_str(),
                )
                .await?
            {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted edit {}", target_event_id);
                if let Some(updated) = self.message_store.get_message(&parent.event_id).await? {
                    self.emit(ProjectorEvent::MessageAnnotationsChanged {
                        site_id: updated.site_id.clone(),
                        page_slug: updated.page_slug.clone(),
                        message: updated,
                    })
                    .await;
                }
            }
            return Ok(());
        }

        info!(
            "Handling redaction for event {} in room {}",
            target_event_id, event.room_id
        );

        // Integrity: only redact a target that actually lives in the room the
        // redaction arrived from. Fetch before redacting so the check uses
        // the same snapshot the deletion will operate on.
        let redacted_at = chrono::DateTime::from_timestamp_millis(event.origin_server_ts)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        let redacted_by = event
            .sender
            .clone()
            .unwrap_or_else(|| event.event_id.clone());

        // 1. Comment message targets.
        if let Some(c) = self.message_store.get_message(&target_event_id).await? {
            if c.room_id != event.room_id {
                warn!(
                    "Ignoring redaction for {} in {}: message lives in {}",
                    target_event_id, event.room_id, c.room_id
                );
                return Ok(());
            }
            if let Some(ref identity) = event.room_identity
                && (c.site_id != identity.site_id || c.page_slug != identity.page_slug)
            {
                warn!(
                    "Ignoring redaction for {} in {}: message belongs to {}/{}",
                    target_event_id, event.room_id, identity.site_id, identity.page_slug
                );
                return Ok(());
            }

            // Deletions issued through the Cumments API embed a signed proof
            // in the redaction's `reason`. When a proof is present it must
            // verify; redactions without one (e.g. manual moderation from a
            // Matrix client) remain governed by the homeserver's
            // authorisation.
            if let Some(proof) = &event.proof
                && !verify_delete_proof(
                    proof,
                    &target_event_id,
                    &c.site_id,
                    &c.page_slug,
                    c.author.public_key.as_deref(),
                )
            {
                warn!(
                    "Rejecting redaction for {} from {}: invalid Cumments delete proof",
                    target_event_id, event.event_id
                );
                return Ok(());
            }

            // Redaction, anti-resurrection tombstone, and delete closure are
            // one atomic local unit. The SSE event follows the commit.
            let outcome = self
                .message_store
                .redact_message_unit(
                    &target_event_id,
                    &event.room_id,
                    redacted_at,
                    &redacted_by,
                    &event.event_id,
                )
                .await?;
            if matches!(
                outcome,
                MessageRedactionOutcome::Redacted | MessageRedactionOutcome::AlreadyRedacted
            ) {
                info!("Successfully redacted message {}", target_event_id);
                self.emit(ProjectorEvent::MessageDeleted {
                    site_id: c.site_id,
                    page_slug: c.page_slug,
                    event_id: target_event_id,
                    submission_id: event.submission_id,
                })
                .await;
            } else {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
            }
            return Ok(());
        }

        // 2. Reaction targets: verify the annotated message lives in this
        // room, then redact the reaction row so it leaves the aggregate.
        if let Some(reaction) = self.message_store.get_reaction(&target_event_id).await? {
            let Some(target) = self
                .message_store
                .get_message(&reaction.message_event_id)
                .await?
            else {
                debug!(
                    "Redaction tombstoned for reaction {}: target message unknown",
                    target_event_id
                );
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                return Ok(());
            };
            if target.room_id != event.room_id {
                warn!(
                    "Ignoring reaction redaction {} in {}: reaction lives in {}",
                    target_event_id, event.room_id, target.room_id
                );
                return Ok(());
            }
            if self
                .message_store
                .redact_reaction(&target_event_id, redacted_at)
                .await?
            {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted reaction {}", target_event_id);
                if let Some(updated) = self
                    .message_store
                    .get_message(&reaction.message_event_id)
                    .await?
                {
                    self.emit(ProjectorEvent::MessageAnnotationsChanged {
                        site_id: updated.site_id.clone(),
                        page_slug: updated.page_slug.clone(),
                        message: updated,
                    })
                    .await;
                }
            }
            return Ok(());
        }

        // 3. Poll votes follow the same annotation rules as reactions.
        if let Some(vote) = self
            .message_store
            .get_poll_vote_by_event(&target_event_id)
            .await?
        {
            let Some(target) = self
                .message_store
                .get_message(&vote.poll_message_id)
                .await?
            else {
                debug!(
                    "Redaction tombstoned for poll vote {}: poll unknown",
                    target_event_id
                );
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                return Ok(());
            };
            if target.room_id != event.room_id {
                warn!(
                    "Ignoring poll vote redaction {} in {}: vote lives in {}",
                    target_event_id, event.room_id, target.room_id
                );
                return Ok(());
            }
            if self
                .message_store
                .redact_poll_vote(&target_event_id, redacted_at, &redacted_by)
                .await?
            {
                self.message_store
                    .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
                    .await?;
                info!("Successfully redacted poll vote {}", target_event_id);
                if let Some(updated) = self
                    .message_store
                    .get_message(&vote.poll_message_id)
                    .await?
                {
                    self.emit(ProjectorEvent::MessageAnnotationsChanged {
                        site_id: updated.site_id.clone(),
                        page_slug: updated.page_slug.clone(),
                        message: updated,
                    })
                    .await;
                }
            }
            return Ok(());
        }

        // Unknown or foreign targets still get a durable tombstone so a later
        // replay cannot resurrect them after the homeserver has applied the
        // redaction.
        self.message_store
            .record_backfill_tombstone(&target_event_id, &event.room_id, &event.event_id)
            .await?;
        Ok(())
    }

    /// Applies homeserver-verified stripped content to a stored state fact and
    /// refreshes derived member/governance projections when it was current.
    async fn apply_stripped_state_redaction(
        &self,
        state: &RoomStateEvent,
        stripped: serde_json::Value,
        redaction_event_id: &str,
        redacted_at_ts: i64,
    ) -> Result<()> {
        self.room_store
            .update_state_event_content(&state.event_id, &stripped)
            .await?;
        self.message_store
            .record_backfill_tombstone(&state.event_id, &state.room_id, redaction_event_id)
            .await?;

        let latest = self
            .room_store
            .get_latest_state_event(&state.room_id, &state.event_type, &state.state_key)
            .await?;
        if latest
            .as_ref()
            .is_some_and(|latest| latest.event_id == state.event_id)
        {
            match state.event_type.as_str() {
                "m.room.member" => {
                    self.room_store
                        .save_member(&RoomMember {
                            room_id: state.room_id.clone(),
                            user_id: state.state_key.clone(),
                            display_name: stripped
                                .get("displayname")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            avatar_url: stripped
                                .get("avatar_url")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            membership: stripped
                                .get("membership")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            updated_at: chrono::DateTime::from_timestamp_millis(redacted_at_ts)
                                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                        })
                        .await?;
                }
                POWER_LEVELS_EVENT_TYPE => {
                    let site = self.site_store.get_site_by_space_id(&state.room_id).await?;
                    let min_level = if site.is_some() {
                        SITE_ROLE_MIN_LEVEL
                    } else {
                        MODERATOR_LEVEL
                    };
                    let roles: Vec<RoleEntry> = role_entries(&stripped, min_level)
                        .into_iter()
                        .filter(|role| !is_as_managed_user(&role.user_id))
                        .collect();
                    if let Some(site) = site {
                        self.governance_store
                            .replace_site_roles(&site.id, &roles)
                            .await?;
                        self.projection_notify.notify_one();
                    } else if matches!(
                        self.registry_store.get_room_status(&state.room_id).await?,
                        Some(RoomStatus::Active)
                    ) {
                        self.governance_store
                            .replace_room_roles(&state.room_id, &roles)
                            .await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Process a space child state event (room added/removed from a Space).
    #[instrument(skip(self))]
    pub async fn process_space_child(&self, event: ParsedSpaceChild) -> Result<()> {
        let site_id = match event.site_id {
            Some(ref id) => id.clone(),
            None => return Ok(()), // Not a managed Space
        };
        let Ok(site_id_val) = SiteId::new(site_id.clone()) else {
            warn!("Ignoring space child for invalid site id {}", site_id);
            return Ok(());
        };

        // AUTO-DISCOVERY: Ensure the site itself exists in the store
        self.site_store
            .ensure_site_exists(site_id_val.as_str(), &event.space_room_id)
            .await?;

        if event.is_attached {
            // Register the child room if we know its identity
            if let Some(ref child_identity) = event.child_room_identity {
                match PageSlug::new(child_identity.page_slug.clone()) {
                    Ok(page_slug) => {
                        self.registry_store
                            .register_room(&event.child_room_id, &site_id_val, &page_slug)
                            .await?;
                        info!(
                            "Registered active room {} for site {}",
                            event.child_room_id, site_id
                        );
                    }
                    Err(_) => warn!(
                        "Ignoring space child with invalid page slug {}",
                        child_identity.page_slug
                    ),
                }
            }
        } else {
            // Space membership is organizational only: a comment room's
            // identity comes from its metadata + canonical alias, not from
            // being linked into a Space, so unlinking does not deactivate it.
            info!(
                "Room {} unlinked from site space {}; registry unchanged",
                event.child_room_id, event.space_room_id
            );
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl cumments_core::ports::StateRedactionRepairer for EventProcessor {
    async fn repair_state_redaction(&self, target_event_id: &str) -> Result<()> {
        let Some(state) = self.room_store.get_state_event(target_event_id).await? else {
            // The local fact disappeared (for example room retirement cleanup);
            // the queue entry no longer has anything to repair.
            self.projection_repair_store
                .resolve_projection_repair(target_event_id)
                .await?;
            return Ok(());
        };
        let Some(driver) = self.driver.as_ref() else {
            anyhow::bail!("state redaction repair requires a Matrix driver");
        };
        let Some(event) = driver.get_event(&state.room_id, target_event_id).await? else {
            anyhow::bail!(
                "homeserver does not expose redacted state event {}",
                target_event_id
            );
        };

        if event.event_id != state.event_id || event.room_id != state.room_id {
            let error = format!(
                "homeserver returned {},{} for repair of {},{}",
                event.event_id, event.room_id, state.event_id, state.room_id
            );
            self.projection_repair_store
                .mark_projection_repair_manual(target_event_id, &error)
                .await?;
            anyhow::bail!(error);
        }
        if event.state_key.as_deref() != Some(state.state_key.as_str())
            || event.event_type != state.event_type
        {
            let error = format!(
                "homeserver event {} has state slot {}/{}, expected {}/{}",
                event.event_id,
                event.event_type,
                event.state_key.unwrap_or_default(),
                state.event_type,
                state.state_key
            );
            self.projection_repair_store
                .mark_projection_repair_manual(target_event_id, &error)
                .await?;
            anyhow::bail!(error);
        }

        let redaction_event_id = match event.redacted_by.as_deref() {
            Some(redaction_event_id) => redaction_event_id,
            None => anyhow::bail!("homeserver event {} is not redacted yet", target_event_id),
        };
        self.apply_stripped_state_redaction(
            &state,
            event.content,
            redaction_event_id,
            event.origin_server_ts,
        )
        .await?;
        self.projection_repair_store
            .resolve_projection_repair(target_event_id)
            .await?;
        info!("Repaired unsupported-version state redaction {target_event_id} from the homeserver");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(display_name: Option<&str>, avatar_url: Option<&str>) -> RoomMember {
        RoomMember {
            room_id: "!room:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: display_name.map(str::to_string),
            avatar_url: avatar_url.map(str::to_string),
            membership: "join".to_string(),
            updated_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        }
    }

    #[test]
    fn matrix_author_snapshot_comes_from_room_member_state() {
        let (display_name, avatar_url) = EventProcessor::author_profile_snapshot(Some(&member(
            Some("Alice"),
            Some("mxc://hs/avatar"),
        )));
        assert_eq!(display_name.as_deref(), Some("Alice"));
        assert_eq!(avatar_url.as_deref(), Some("mxc://hs/avatar"));
    }

    #[test]
    fn author_without_member_state_has_no_profile() {
        let (display_name, avatar_url) = EventProcessor::author_profile_snapshot(None);
        assert!(display_name.is_none());
        assert!(avatar_url.is_none());
    }

    #[test]
    fn visitor_author_profile_comes_from_room_member_state() {
        let (display_name, avatar_url) = EventProcessor::author_profile_snapshot(Some(&member(
            Some("访客"),
            Some("mxc://hs/avatar"),
        )));
        assert_eq!(display_name.as_deref(), Some("访客"));
        assert_eq!(avatar_url.as_deref(), Some("mxc://hs/avatar"));
    }
}
