//! Stable identifiers for the externally visible capability surface.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    Low,
    Medium,
    High,
    Severe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleKind {
    Sync,
    Accepted,
    DurableIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditRequirement {
    None,
    RequestTrace,
    Required,
}

pub struct Capability {
    pub id: &'static str,
    pub summary: &'static str,
    pub risk: RiskTier,
    pub lifecycle: LifecycleKind,
    pub audit: AuditRequirement,
}

const fn capability(
    id: &'static str,
    summary: &'static str,
    risk: RiskTier,
    lifecycle: LifecycleKind,
    audit: AuditRequirement,
) -> Capability {
    Capability {
        id,
        summary,
        risk,
        lifecycle,
        audit,
    }
}

pub const CAPABILITIES: &[Capability] = &[
    capability(
        "health.read",
        "Read instance health.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "visitor.challenge.issue",
        "Issue a visitor PoW challenge.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::RequestTrace,
    ),
    capability(
        "visitor.comment.create",
        "Submit a new comment.",
        RiskTier::Medium,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.comment.update",
        "Submit or apply a comment edit.",
        RiskTier::Medium,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.comment.delete",
        "Submit or apply a comment redaction.",
        RiskTier::High,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.comment.list",
        "Query the public comment read model.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "visitor.stream.subscribe",
        "Subscribe to live comment events.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "site.register",
        "Register a site and bootstrap ownership.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.retirement.start",
        "Start asynchronous site retirement.",
        RiskTier::Severe,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "site.retirement.read",
        "Read asynchronous site retirement status.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "site.verification.start",
        "Start origin verification.",
        RiskTier::Medium,
        LifecycleKind::Accepted,
        AuditRequirement::RequestTrace,
    ),
    capability(
        "site.verification.confirm",
        "Confirm origin verification.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.secret.issue",
        "Issue a site backend HMAC secret.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.secret.rotate",
        "Rotate a site backend HMAC secret.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.secret.revoke",
        "Revoke a site backend HMAC secret.",
        RiskTier::Severe,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.origin.revoke",
        "Revoke a verified site origin.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "site.config.export",
        "Export declarative configuration for a DB site.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "site.claim_token.rotate",
        "Rotate a site claim token.",
        RiskTier::Severe,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "operator.site.list",
        "List effective sites known to this instance.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "governance.admin.claim",
        "Start a site administrator claim.",
        RiskTier::High,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "governance.admin.remove",
        "Remove a pending or applied administrator.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "governance.manager.claim",
        "Start a site manager claim.",
        RiskTier::Medium,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "governance.manager.remove",
        "Remove a pending or applied manager.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "governance.moderator.list",
        "List page moderators.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "governance.moderator.claim",
        "Start a page moderator claim.",
        RiskTier::Medium,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "governance.moderator.remove",
        "Remove a pending or applied moderator.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "governance.roles.list",
        "Read projected governance roles.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "governance.ownership.transfer.start",
        "Start site ownership transfer.",
        RiskTier::Severe,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "sticker.list",
        "List stickers available to a site.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "sticker.add",
        "Add a sticker to a site pack.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "sticker.remove",
        "Remove a sticker from a site pack.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "room.info.read",
        "Read public page room information.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "room.quarantine.list",
        "List quarantined rooms.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "room.quarantine.reinstate",
        "Reinstate a quarantined room.",
        RiskTier::High,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "room.upgrade.start",
        "Start a Matrix room upgrade.",
        RiskTier::High,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "room.upgrade.intent.list",
        "List durable room upgrade intents.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "room.upgrade.intent.recover",
        "Recover a reviewed room upgrade intent.",
        RiskTier::High,
        LifecycleKind::DurableIntent,
        AuditRequirement::Required,
    ),
    capability(
        "room.retirement.start",
        "Start asynchronous room retirement.",
        RiskTier::High,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "room.retirement.read",
        "Read asynchronous room retirement status.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "page.retirement.start",
        "Start page comment-room retirement.",
        RiskTier::High,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "page.retirement.read",
        "Read page comment-room retirement status.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "media.proxy",
        "Proxy authorized Matrix media.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "visitor.media.upload",
        "Upload media for a comment.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.reaction.add",
        "Add a reaction to a comment.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.poll.vote",
        "Vote in a poll.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.location.create",
        "Submit a location comment.",
        RiskTier::Medium,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.avatar.set",
        "Set a visitor avatar.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.avatar.delete",
        "Delete a visitor avatar.",
        RiskTier::Medium,
        LifecycleKind::Sync,
        AuditRequirement::Required,
    ),
    capability(
        "visitor.profile.read",
        "Read a visitor profile.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "projection.repair.list",
        "List durable projection repairs.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "projection.repair.read",
        "Read one durable projection repair.",
        RiskTier::Low,
        LifecycleKind::Sync,
        AuditRequirement::None,
    ),
    capability(
        "projection.repair.retry",
        "Requeue a pending or manual projection repair.",
        RiskTier::High,
        LifecycleKind::Accepted,
        AuditRequirement::Required,
    ),
];

pub struct HttpOperation {
    pub method: &'static str,
    pub path: &'static str,
    pub operation_id: &'static str,
    pub capability_id: &'static str,
}

const fn http_operation(
    method: &'static str,
    path: &'static str,
    operation_id: &'static str,
    capability_id: &'static str,
) -> HttpOperation {
    HttpOperation {
        method,
        path,
        operation_id,
        capability_id,
    }
}

pub const HTTP_OPERATIONS: &[HttpOperation] = &[
    http_operation("GET", "/health", "health", "health.read"),
    http_operation(
        "GET",
        "/api/v1/challenge",
        "getChallenge",
        "visitor.challenge.issue",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/comments",
        "postComment",
        "visitor.comment.create",
    ),
    http_operation(
        "QUERY",
        "/api/v1/sites/{site_id}/pages/{page_slug}/comments",
        "queryComments",
        "visitor.comment.list",
    ),
    http_operation(
        "PATCH",
        "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}",
        "updateCommentPath",
        "visitor.comment.update",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}",
        "deleteCommentPath",
        "visitor.comment.delete",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/pages/{page_slug}/sse",
        "commentSse",
        "visitor.stream.subscribe",
    ),
    http_operation("POST", "/api/v1/sites", "registerSite", "site.register"),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/retirement",
        "startSiteRetirement",
        "site.retirement.start",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/retirement",
        "getSiteRetirement",
        "site.retirement.read",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/verifications",
        "startVerification",
        "site.verification.start",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/verifications/confirm",
        "confirmVerification",
        "site.verification.confirm",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/secret",
        "issueSecret",
        "site.secret.issue",
    ),
    http_operation(
        "QUERY",
        "/api/v1/operator/sites",
        "listOperatorSites",
        "operator.site.list",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/retirement",
        "operatorStartSiteRetirement",
        "site.retirement.start",
    ),
    http_operation(
        "GET",
        "/api/v1/operator/sites/{site_id}/retirement",
        "operatorGetSiteRetirement",
        "site.retirement.read",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/origins/revoke",
        "revokeVerifiedOrigin",
        "site.origin.revoke",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/secret/rotate",
        "rotateSecret",
        "site.secret.rotate",
    ),
    http_operation(
        "DELETE",
        "/api/v1/operator/sites/{site_id}/secret",
        "revokeSecret",
        "site.secret.revoke",
    ),
    http_operation(
        "GET",
        "/api/v1/operator/sites/{site_id}/config-snippet",
        "configSnippet",
        "site.config.export",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/claim-token/rotate",
        "rotateClaimToken",
        "site.claim_token.rotate",
    ),
    http_operation(
        "QUERY",
        "/api/v1/operator/quarantined-rooms",
        "listQuarantinedRooms",
        "room.quarantine.list",
    ),
    http_operation(
        "DELETE",
        "/api/v1/operator/quarantined-rooms/{room_id}",
        "reinstateQuarantinedRoom",
        "room.quarantine.reinstate",
    ),
    http_operation(
        "QUERY",
        "/api/v1/operator/room-upgrade-intents",
        "listRoomUpgradeIntents",
        "room.upgrade.intent.list",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/room-upgrade-intents/{room_id}/recoveries",
        "createRoomUpgradeRecovery",
        "room.upgrade.intent.recover",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/rooms/{room_id}/upgrades",
        "createRoomUpgrade",
        "room.upgrade.start",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/rooms/{room_id}/retirement",
        "operatorStartRoomRetirement",
        "room.retirement.start",
    ),
    http_operation(
        "GET",
        "/api/v1/operator/rooms/{room_id}/retirement",
        "operatorGetRoomRetirement",
        "room.retirement.read",
    ),
    http_operation(
        "GET",
        "/api/v1/media/{server}/{media_id}",
        "proxyMedia",
        "media.proxy",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/pages/{page_slug}/room",
        "getRoomInfo",
        "room.info.read",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/media",
        "uploadMedia",
        "visitor.media.upload",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/stickers",
        "listStickers",
        "sticker.list",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/packs/{pack_id}/stickers",
        "addSiteSticker",
        "sticker.add",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/packs/{pack_id}/stickers",
        "removeSiteSticker",
        "sticker.remove",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/packs/{pack_id}/stickers",
        "operatorAddSiteSticker",
        "sticker.add",
    ),
    http_operation(
        "DELETE",
        "/api/v1/operator/sites/{site_id}/packs/{pack_id}/stickers",
        "operatorRemoveSiteSticker",
        "sticker.remove",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/comments/{comment_id}/reactions",
        "reactToComment",
        "visitor.reaction.add",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/polls/{poll_id}/votes",
        "votePoll",
        "visitor.poll.vote",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/location",
        "postLocation",
        "visitor.location.create",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/upgrades",
        "createPageRoomUpgrade",
        "room.upgrade.start",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/retirement",
        "startPageRetirement",
        "page.retirement.start",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/pages/{page_slug}/retirement",
        "getPageRetirement",
        "page.retirement.read",
    ),
    http_operation(
        "PUT",
        "/api/v1/sites/{site_id}/visitors/avatar",
        "setVisitorAvatar",
        "visitor.avatar.set",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/visitors/avatar",
        "deleteVisitorAvatar",
        "visitor.avatar.delete",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/visitors/profile",
        "getVisitorProfile",
        "visitor.profile.read",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/admins",
        "addSiteAdmin",
        "governance.admin.claim",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/admins",
        "removeSiteAdmin",
        "governance.admin.remove",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/managers",
        "addSiteManager",
        "governance.manager.claim",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/managers",
        "removeSiteManager",
        "governance.manager.remove",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/pages/{page_slug}/moderators",
        "listRoomModerators",
        "governance.moderator.list",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/pages/{page_slug}/moderators",
        "addRoomModerator",
        "governance.moderator.claim",
    ),
    http_operation(
        "DELETE",
        "/api/v1/sites/{site_id}/pages/{page_slug}/moderators",
        "removeRoomModerator",
        "governance.moderator.remove",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/pages/{page_slug}/roles",
        "listPageRoles",
        "governance.roles.list",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/claim-token/rotate",
        "rotateSiteClaimToken",
        "site.claim_token.rotate",
    ),
    http_operation(
        "POST",
        "/api/v1/sites/{site_id}/ownership/transfer",
        "startSiteOwnershipTransfer",
        "governance.ownership.transfer.start",
    ),
    http_operation(
        "GET",
        "/api/v1/sites/{site_id}/roles",
        "listSiteRoles",
        "governance.roles.list",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/admins",
        "operatorAddSiteAdmin",
        "governance.admin.claim",
    ),
    http_operation(
        "DELETE",
        "/api/v1/operator/sites/{site_id}/admins",
        "operatorRemoveSiteAdmin",
        "governance.admin.remove",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/managers",
        "operatorAddSiteManager",
        "governance.manager.claim",
    ),
    http_operation(
        "DELETE",
        "/api/v1/operator/sites/{site_id}/managers",
        "operatorRemoveSiteManager",
        "governance.manager.remove",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/sites/{site_id}/ownership/transfer",
        "operatorStartSiteOwnershipTransfer",
        "governance.ownership.transfer.start",
    ),
    http_operation(
        "QUERY",
        "/api/v1/operator/projection-repairs",
        "listProjectionRepairs",
        "projection.repair.list",
    ),
    http_operation(
        "GET",
        "/api/v1/operator/projection-repairs/{target_event_id}",
        "getProjectionRepair",
        "projection.repair.read",
    ),
    http_operation(
        "POST",
        "/api/v1/operator/projection-repairs/{target_event_id}/retry",
        "retryProjectionRepair",
        "projection.repair.retry",
    ),
];

pub fn find_capability(id: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|entry| entry.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn capability_ids_are_unique_and_referenceable() {
        let unique: HashSet<_> = CAPABILITIES.iter().map(|item| item.id).collect();
        assert_eq!(unique.len(), CAPABILITIES.len());

        for entry in CAPABILITIES {
            assert_eq!(
                find_capability(entry.id).map(|found| found.id),
                Some(entry.id)
            );
        }
    }

    #[test]
    fn http_operations_are_unique_and_bound_to_known_capabilities() {
        let mut keys = HashMap::new();
        for operation in HTTP_OPERATIONS {
            let key = (operation.method, operation.path);
            assert!(
                keys.insert(key, operation.operation_id).is_none(),
                "duplicate HTTP operation {} {}",
                operation.method,
                operation.path
            );
            assert!(find_capability(operation.capability_id).is_some());
        }

        let operation_ids: HashSet<_> = HTTP_OPERATIONS.iter().map(|op| op.operation_id).collect();
        assert_eq!(operation_ids.len(), HTTP_OPERATIONS.len());
        assert_eq!(HTTP_OPERATIONS.len(), 64);
    }
}
