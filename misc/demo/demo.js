            // ==========================================
            // 全局状态
            // ==========================================

            const SETTINGS_KEY = "cumments_demo_settings";
            const IDENTITY_KEY = "cumments_identity";
            const MNEMONIC_SESSION_KEY = "cumments_mnemonic_session";
            const AVATAR_CACHE_PREFIX = "cumments_avatar_";
            const AVATAR_MAX_BYTES = 5 * 1024 * 1024;
            const AVATAR_MAX_DIMENSION = 512;

            // ==========================================
            // BIP39
            // ==========================================

            // file:// 页面无法使用静态 ES module，因此经典脚本通过动态
            // import() 加载 CDN 上的 ESM 包；失败或超时则回退随机身份。
            const BIP39_CDN = [
                "https://cdn.jsdelivr.net/npm/@scure/bip39@2.3.0/+esm",
                "https://cdn.jsdelivr.net/npm/@scure/bip39@2.3.0/wordlists/english/+esm",
            ];

            async function loadBip39() {
                try {
                    const [bip39, wordlist] = await Promise.all([
                        import(BIP39_CDN[0]),
                        import(BIP39_CDN[1]),
                    ]);
                    window.bip39 = {
                        generateMnemonic: bip39.generateMnemonic,
                        mnemonicToSeedSync: bip39.mnemonicToSeedSync,
                        validateMnemonic: bip39.validateMnemonic,
                    };
                    window.bip39Wordlist = wordlist.wordlist;
                } catch {
                    window.bip39 = null;
                    window.bip39Wordlist = null;
                }
            }

            function withTimeout(promise, ms) {
                return Promise.race([
                    promise,
                    new Promise((_, reject) =>
                        setTimeout(() => reject(new Error("timeout")), ms),
                    ),
                ]);
            }

            // ==========================================
            // 语言 / i18n
            // ==========================================

            const I18N = {
                zh: {
                    title: "Cumments 评论区演示",
                    brand_subtitle: "基于 Matrix 的评论系统",
                    settings: "设置",
                    article_title: "这是一篇用来验证评论功能的示例文章",
                    article_p1:
                        "下面的评论区会连接真实的 Cumments 后端，支持发布、编辑、删除本人评论，以及通过 SSE 接收其他访客的实时更新。",
                    article_p2:
                        "你的身份是浏览器本地生成的 Ed25519 密钥对：公钥即身份，私钥不会离开浏览器。评论会以 Matrix 事件的形式写入房间，SQLite 只是可重建的读模型。",
                    comments: "评论",
                    sse_connected: "实时连接",
                    sse_disconnected: "未连接",
                    tab_all: "全部",
                    tab_mine: "我的评论",
                    placeholder_name: "昵称",
                    replying: "正在回复",
                    cancel: "取消",
                    placeholder_content: "写下你的评论，支持 Markdown…",
                    pow_ready: "PoW 就绪",
                    publish: "发布评论",
                    loading: "加载中…",
                    prev_page: "上一页",
                    next_page: "下一页",
                    manage_note:
                        "你只能编辑/删除自己发布的评论；管理员可以直接在 Matrix 客户端中治理评论房间。",
                    api_url: "API 地址",
                    site_id: "Site ID",
                    slug: "Slug",
                    identity_public_key: "身份公钥（Ed25519）",
                    copy: "复制",
                    show_mnemonic: "显示助记词",
                    export_private_key: "导出私钥",
                    restore_mnemonic: "助记词恢复",
                    import_private_key: "导入私钥",
                    edited: "已编辑",
                    placeholder_mnemonic: "输入 12 个英文助记词（空格分隔）…",
                    restore_identity: "恢复此身份",
                    reset_identity: "重置身份",
                    identity_note:
                        "身份 = Ed25519 公钥。私钥只保存在本浏览器；助记词不跨会话保存、仅在创建当次会话可查看。长期备份请抄写助记词或导出私钥；恢复/导入/重置后，旧公钥发布的评论将不再显示编辑/删除入口。",
                    avatar_label: "头像",
                    avatar_upload: "上传头像",
                    avatar_remove: "移除头像",
                    avatar_note:
                        "头像按站点独立（虚拟用户由站点 + 公钥派生），只接受图片且会自动压缩到 512px 以内。",
                    avatar_uploaded: "头像已更新",
                    avatar_removed: "头像已移除",
                    avatar_upload_failed: "头像上传失败：",
                    avatar_remove_failed: "头像移除失败：",
                    avatar_bad_type: "请选择图片文件",
                    avatar_too_large: "图片不能超过 5MB",
                    save_refresh: "保存并刷新",
                    mnemonic_title_backup: "备份你的身份助记词",
                    mnemonic_title_view: "身份助记词（本次会话）",
                    mnemonic_desc_backup:
                        "这是新身份唯一的恢复凭证。它不会跨会话保存，也不会发送到任何服务器；请用纸笔抄写并妥善保管，丢失后只能用导出的私钥 JSON 恢复。",
                    mnemonic_desc_view:
                        "助记词仅在创建当次会话中可再查看，关闭标签页后即消失。长期备份请抄写，或使用导出私钥。",
                    copy_mnemonic: "复制助记词",
                    mnemonic_done: "我已备份，继续",
                    close: "关闭",
                    mnemonic_alt: "不想用助记词？改用随机私钥身份",
                    guest_default: "访客",
                    matrix_user: "Matrix 用户",
                    guest_badge: "Cumments 访客",
                    identity_tag_title: "同一访客的稳定身份标记；改名不会改变它",
                    role_owner: "站主",
                    role_co_manager: "协管员",
                    role_moderator: "版主",
                    room_roles: "治理",
                    reply: "回复",
                    edit: "编辑",
                    delete: "删除",
                    save_edit: "保存修改",
                    depth_limit: "回复深度已达上限",
                    page_info: "显示 {start}-{end} 条（共 {total} 条）",
                    no_data: "暂无数据",
                    empty_all: "还没有评论，来抢沙发吧。",
                    empty_mine: "你还没有在这篇文章下发表过评论。",
                    load_failed: "加载失败：",
                    err_identity_init: "身份初始化失败：",
                    err_unknown: "未知错误",
                    err_identity_invalid: "本地身份数据无效，已为你生成新身份",
                    err_identity_save: "身份保存失败，请检查浏览器存储权限",
                    err_bip39_load: "助记词库未加载，请检查网络后重试",
                    err_mnemonic_invalid: "助记词无效，请检查拼写",
                    mnemonic_fallback_toast:
                        "助记词库未能加载，已生成随机身份；可稍后重置身份以启用助记词恢复",
                    mnemonic_copied: "助记词已复制",
                    mnemonic_copy_failed: "复制失败，请手动抄写",
                    mnemonic_view_notice:
                        "助记词仅在创建当次会话可查看；未备份请用导出私钥保存身份",
                    err_mnemonic_input: "请输入助记词",
                    mnemonic_same: "助记词与当前身份一致，无需恢复",
                    confirm_restore:
                        "恢复后将切换为该助记词对应的身份，当前身份发布的评论将失去编辑/删除入口。确定继续？",
                    mnemonic_restored: "身份已从助记词恢复",
                    err_identity_not_ready: "身份尚未就绪",
                    private_key_exported: "私钥已导出为 JSON 文件",
                    err_import_format:
                        "文件格式不正确（需要 version/publicKey/privateKey）",
                    err_import_mismatch: "私钥与公钥不匹配，已拒绝导入",
                    import_current: "导入的是当前身份",
                    confirm_import:
                        "导入后将切换为该私钥对应的身份，当前身份发布的评论将失去编辑/删除入口。确定继续？",
                    private_key_imported: "私钥已导入",
                    err_import_fail: "导入失败",
                    confirm_reset:
                        "重置身份后，之前用旧公钥发布的评论将无法在此浏览器中编辑/删除。确定继续？",
                    public_key_copied: "公钥已复制",
                    identity_created: "身份已创建，助记词请妥善保管",
                    err_try_again: "可先复制助记词，稍后再试",
                    identity_random: "已改用随机私钥身份，仍可在设置中导出私钥备份",
                    err_random_identity: "创建随机身份失败：",
                    err_content_empty: "写点什么再发布吧",
                    status_fetch_challenge: "获取 PoW 挑战…",
                    status_computing_pow: "计算 PoW（难度 {difficulty}）…",
                    status_signing: "签名中…",
                    status_submitting: "提交中…",
                    status_submitted: "提交成功，等待 Matrix 同步…",
                    status_synced: "已同步到 Matrix",
                    status_still_waiting: "仍在等待 Matrix 同步，可手动刷新查看…",
                    comment_submitted: "评论已提交，等待同步",
                    comment_synced: "评论已同步到 Matrix",
                    status_failed: "发送失败",
                    publish_failed: "发布失败：",
                    err_content_empty_edit: "评论内容不能为空",
                    comment_updated: "评论已更新",
                    edit_failed: "编辑失败：",
                    confirm_delete: "确定删除这条评论吗？",
                    comment_deleted: "评论已删除",
                    delete_failed: "删除失败：",
                    deleted_message: "这条消息已删除",
                    encrypted_message: "加密消息（无法显示内容）",
                    unsupported_message: "不支持的消息类型",
                    download: "下载",
                    open_map: "在地图中打开",
                    votes: "票",
                    members: "成员",
                    members_online: "人在线",
                    member_joined: "加入了房间",
                    member_left: "离开了房间",
                    member_renamed: "改了名字",
                    room_name_changed: "修改了房间名：",
                    room_topic_changed: "更新了话题：",
                    room_avatar_changed: "更换了房间头像",
                    typing_now: "正在输入…",
                    add_image: "图片",
                    record_voice: "语音",
                    stop_record: "停止",
                    upload_media_failed: "媒体上传失败：",
                    stickers: "贴纸",
                    attach_file: "文件",
                    no_stickers: "站点暂无贴纸",
                    reaction_submitted: "已发送回应",
                    vote_submitted: "投票已提交",
                    location: "位置",
                    location_submitted: "位置已发送",
                    sse_new_comment: "新评论：",
                    sse_comment_updated: "评论已更新：",
                    sse_comment_deleted: "有评论被删除",
                    just_now: "刚刚",
                    minutes_ago: "{n} 分钟前",
                    hours_ago: "{n} 小时前",
                    days_ago: "{n} 天前",
                },
                en: {
                    title: "Cumments comment section demo",
                    brand_subtitle: "A comment system powered by Matrix",
                    settings: "Settings",
                    article_title: "A sample article for trying out the comment section",
                    article_p1:
                        "The comment section below connects to a real Cumments backend: post, edit and delete your own comments, and receive live updates from other visitors over SSE.",
                    article_p2:
                        "Your identity is an Ed25519 key pair generated locally in the browser: the public key is your identity and the private key never leaves the browser. Comments are written as Matrix events; SQLite is only a rebuildable read model.",
                    comments: "Comments",
                    sse_connected: "Live",
                    sse_disconnected: "Offline",
                    tab_all: "All",
                    tab_mine: "Mine",
                    placeholder_name: "Name",
                    replying: "Replying to",
                    cancel: "Cancel",
                    placeholder_content: "Write a comment, Markdown supported…",
                    pow_ready: "PoW ready",
                    publish: "Publish",
                    loading: "Loading…",
                    prev_page: "Prev",
                    next_page: "Next",
                    manage_note:
                        "You can only edit or delete your own comments; moderators can govern comment rooms directly in a Matrix client.",
                    api_url: "API URL",
                    site_id: "Site ID",
                    slug: "Slug",
                    identity_public_key: "Identity public key (Ed25519)",
                    copy: "Copy",
                    show_mnemonic: "Show mnemonic",
                    export_private_key: "Export private key",
                    restore_mnemonic: "Restore mnemonic",
                    import_private_key: "Import private key",
                    edited: "edited",
                    placeholder_mnemonic:
                        "Enter the 12-word English mnemonic (space-separated)…",
                    restore_identity: "Restore identity",
                    reset_identity: "Reset identity",
                    identity_note:
                        "Identity = Ed25519 public key. The private key only lives in this browser; the mnemonic is not persisted across sessions and is only viewable in the session that created it. For long-term backup, write down the mnemonic or export the private key; after restore/import/reset, comments posted by the old public key lose their edit/delete controls.",
                    avatar_label: "Avatar",
                    avatar_upload: "Upload avatar",
                    avatar_remove: "Remove avatar",
                    avatar_note:
                        "Avatars are per-site (the virtual user is derived from site + public key), images only, and are downscaled to at most 512px.",
                    avatar_uploaded: "Avatar updated",
                    avatar_removed: "Avatar removed",
                    avatar_upload_failed: "Avatar upload failed: ",
                    avatar_remove_failed: "Avatar removal failed: ",
                    avatar_bad_type: "Please choose an image file",
                    avatar_too_large: "Images must be under 5MB",
                    save_refresh: "Save & refresh",
                    mnemonic_title_backup: "Back up your identity mnemonic",
                    mnemonic_title_view: "Identity mnemonic (this session)",
                    mnemonic_desc_backup:
                        "This is the only recovery credential for the new identity. It is not persisted across sessions and is never sent to any server; write it down and keep it safe. If you lose it, only an exported private-key JSON can recover the identity.",
                    mnemonic_desc_view:
                        "The mnemonic is only viewable in the session that created it and disappears when the tab closes. For long-term backup, write it down or export the private key.",
                    copy_mnemonic: "Copy mnemonic",
                    mnemonic_done: "I've backed it up, continue",
                    close: "Close",
                    mnemonic_alt: "Don't want a mnemonic? Use a random identity instead",
                    guest_default: "Guest",
                    matrix_user: "Matrix user",
                    guest_badge: "Cumments guest",
                    identity_tag_title: "Stable identity marker for the same guest; renaming does not change it",
                    role_owner: "Owner",
                    role_co_manager: "Co-manager",
                    role_moderator: "Moderator",
                    room_roles: "Governance",
                    reply: "Reply",
                    edit: "Edit",
                    delete: "Delete",
                    save_edit: "Save changes",
                    depth_limit: "Reply depth limit reached",
                    page_info: "Showing {start}-{end} of {total}",
                    no_data: "No comments",
                    empty_all: "No comments yet — be the first!",
                    empty_mine: "You haven't commented on this article yet.",
                    load_failed: "Failed to load: ",
                    err_identity_init: "Identity initialization failed: ",
                    err_unknown: "Unknown error",
                    err_identity_invalid:
                        "Stored identity is invalid; a new identity was generated",
                    err_identity_save:
                        "Failed to save identity; check browser storage permissions",
                    err_bip39_load: "Mnemonic library not loaded; check the network and retry",
                    err_mnemonic_invalid: "Invalid mnemonic; check the spelling",
                    mnemonic_fallback_toast:
                        "Mnemonic library failed to load; a random identity was generated. You can reset the identity later to enable mnemonic recovery",
                    mnemonic_copied: "Mnemonic copied",
                    mnemonic_copy_failed: "Copy failed; write it down manually",
                    mnemonic_view_notice:
                        "The mnemonic is only viewable in the session that created it; if you haven't backed it up, export the private key instead",
                    err_mnemonic_input: "Enter the mnemonic",
                    mnemonic_same: "This mnemonic matches the current identity; nothing to restore",
                    confirm_restore:
                        "Restoring switches to the identity derived from this mnemonic; comments posted by the current identity will lose their edit/delete controls. Continue?",
                    mnemonic_restored: "Identity restored from mnemonic",
                    err_identity_not_ready: "Identity not ready yet",
                    private_key_exported: "Private key exported as a JSON file",
                    err_import_format:
                        "Invalid file format (expected version/publicKey/privateKey)",
                    err_import_mismatch:
                        "Private key does not match the public key; import rejected",
                    import_current: "That's the current identity",
                    confirm_import:
                        "Importing switches to the identity for this private key; comments posted by the current identity will lose their edit/delete controls. Continue?",
                    private_key_imported: "Private key imported",
                    err_import_fail: "Import failed",
                    confirm_reset:
                        "After resetting, comments posted with the old public key can no longer be edited or deleted in this browser. Continue?",
                    public_key_copied: "Public key copied",
                    identity_created: "Identity created; keep the mnemonic safe",
                    err_try_again: "Copy the mnemonic first, then try again",
                    identity_random:
                        "Switched to a random identity; you can still export the private key in settings",
                    err_random_identity: "Failed to create a random identity: ",
                    err_content_empty: "Write something before publishing",
                    status_fetch_challenge: "Fetching PoW challenge…",
                    status_computing_pow: "Computing PoW (difficulty {difficulty})…",
                    status_signing: "Signing…",
                    status_submitting: "Submitting…",
                    status_submitted: "Submitted; waiting for Matrix sync…",
                    status_synced: "Synced to Matrix",
                    status_still_waiting: "Still waiting for Matrix sync; try refreshing manually…",
                    comment_submitted: "Comment submitted; waiting for sync",
                    comment_synced: "Comment synced to Matrix",
                    status_failed: "Send failed",
                    publish_failed: "Publish failed: ",
                    err_content_empty_edit: "Comment content cannot be empty",
                    comment_updated: "Comment updated",
                    edit_failed: "Edit failed: ",
                    confirm_delete: "Delete this comment?",
                    comment_deleted: "Comment deleted",
                    delete_failed: "Delete failed: ",
                    deleted_message: "This message was deleted",
                    encrypted_message: "Encrypted message (content unavailable)",
                    unsupported_message: "Unsupported message type",
                    download: "Download",
                    open_map: "Open in map",
                    votes: "votes",
                    members: "members",
                    members_online: "online",
                    member_joined: "joined the room",
                    member_left: "left the room",
                    member_renamed: "changed name",
                    room_name_changed: "changed the room name to: ",
                    room_topic_changed: "updated the topic: ",
                    room_avatar_changed: "changed the room avatar",
                    typing_now: "is typing…",
                    add_image: "Image",
                    record_voice: "Voice",
                    stop_record: "Stop",
                    upload_media_failed: "Media upload failed: ",
                    stickers: "Stickers",
                    attach_file: "File",
                    no_stickers: "No stickers in the site's packs",
                    reaction_submitted: "Reaction sent",
                    vote_submitted: "Vote submitted",
                    location: "Location",
                    location_submitted: "Location sent",
                    sse_new_comment: "New comment: ",
                    sse_comment_updated: "Comment updated: ",
                    sse_comment_deleted: "A comment was deleted",
                    just_now: "just now",
                    minutes_ago: "{n} minutes ago",
                    hours_ago: "{n} hours ago",
                    days_ago: "{n} days ago",
                },
            };

            let lang = "zh";
            try {
                lang = localStorage.getItem("cumments_demo_lang") || "zh";
            } catch {
                // storage unavailable; keep the default language
            }
            if (!I18N[lang]) lang = "zh";

            function t(key, params) {
                let text =
                    (I18N[lang] && I18N[lang][key]) || I18N.zh[key] || key;
                if (params) {
                    for (const [name, value] of Object.entries(params)) {
                        text = text.replaceAll(`{${name}}`, value);
                    }
                }
                return text;
            }

            let sseConnected = false;

            function toggleLang() {
                lang = lang === "en" ? "zh" : "en";
                try {
                    localStorage.setItem("cumments_demo_lang", lang);
                } catch {
                    // keep the in-memory choice
                }
                applyI18n();
            }

            function applyI18n() {
                document.documentElement.lang = lang === "en" ? "en" : "zh-CN";
                document.title = t("title");
                document.querySelectorAll("[data-i18n]").forEach((el) => {
                    el.textContent = t(el.dataset.i18n);
                });
                document
                    .querySelectorAll("[data-i18n-placeholder]")
                    .forEach((el) => {
                        el.placeholder = t(el.dataset.i18nPlaceholder);
                    });
                const langBtn = document.getElementById("langBtn");
                if (langBtn) langBtn.textContent = lang === "en" ? "中文" : "EN";
                const sseText = document.getElementById("sseText");
                if (sseText) {
                    sseText.textContent = sseConnected
                        ? t("sse_connected")
                        : t("sse_disconnected");
                }
                const powStatus = document.getElementById("powStatus");
                if (powStatus) {
                    powStatus.textContent = state.pendingComment
                        ? t("status_submitted")
                        : t("pow_ready");
                }
                if (state.meta) updatePagination(state.meta);
                updateReplyBanner();
                updateComposerAvatar();
                const modal = document.getElementById("mnemonicModal");
                if (modal && !modal.classList.contains("hidden")) {
                    showMnemonicModal(currentMnemonic || "", mnemonicModalMode);
                }
            }

            const DEFAULT_SETTINGS = {
                api: "http://localhost:7931",
                siteId: "my-blog",
                slug: "hello-world",
                displayName: "",
            };

            const state = {
                currentSse: null,
                currentPage: 1,
                perPage: 10,
                filter: "all",
                meta: null,
                allComments: [],
                mineComments: [],
                mineTotal: 0,
                replyingTo: null,
                pendingComment: null,
                presenceOnline: new Set(),
                siteOwners: new Set(),
                siteCoManagers: new Set(),
                roomModerators: new Set(),
            };

            let identity = null;
            let pendingIdentity = null;
            let mnemonicModalMode = "backup";
            let currentMnemonic = null;
            let pendingPollTimer = null;
            let sseKey = "";
            let ownAvatarUrl = null;
            let ownAvatarSiteId = null;
            const PENDING_POLL_INTERVAL_MS = 2000;
            const PENDING_POLL_LIMIT = 15;
            const PENDING_POLL_LONG_INTERVAL_MS = 10000;

            // ==========================================
            // 初始化
            // ==========================================

            document.addEventListener("DOMContentLoaded", async () => {
                applyI18n();
                loadSettings();
                await withTimeout(loadBip39(), 3000).catch(() => {
                    window.bip39 = null;
                    window.bip39Wordlist = null;
                });
                try {
                    const id = await ensureIdentity();
                    identity = id;
                    renderIdentity();
                    await initApp();
                    refreshOwnProfile();
                } catch (e) {
                    renderError(
                        new Error(
                            t("err_identity_init") +
                                (e.message || t("err_unknown")),
                        ),
                    );
                }
            });

            function loadSettings() {
                let saved = {};
                try {
                    saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}");
                } catch {
                    saved = {};
                }
                const s = { ...DEFAULT_SETTINGS, ...saved };
                document.getElementById("apiUrl").value = s.api;
                document.getElementById("siteId").value = s.siteId;
                document.getElementById("slug").value = s.slug;
                document.getElementById("settingDisplayName").value = s.displayName;
                syncComposer(s);
            }

            function getSettings() {
                return {
                    api: document.getElementById("apiUrl").value.trim().replace(/\/+$/, ""),
                    siteId: document.getElementById("siteId").value.trim(),
                    slug: document.getElementById("slug").value.trim(),
                    displayName:
                        document.getElementById("settingDisplayName").value.trim() ||
                        t("guest_default"),
                };
            }

            function syncComposer(s) {
                document.getElementById("composerDisplayName").value = s.displayName;
                updateComposerAvatar();
            }

            function saveSettings() {
                const s = getSettings();
                localStorage.setItem(SETTINGS_KEY, JSON.stringify(s));
                syncComposer(s);
                closeSettings();
                const nextKey = `${s.api}|${s.siteId}|${s.slug}`;
                if (nextKey !== sseKey) {
                    sseKey = nextKey;
                    initApp();
                }
            }

            function openSettings() {
                const s = getSettings();
                document.getElementById("apiUrl").value = s.api;
                document.getElementById("siteId").value = s.siteId;
                document.getElementById("slug").value = s.slug;
                const composerDisplayName = document
                    .getElementById("composerDisplayName")
                    .value.trim();
                document.getElementById("settingDisplayName").value =
                    composerDisplayName || s.displayName;
                renderIdentity();
                refreshOwnProfile();
                document.getElementById("settingsModal").classList.remove("hidden");
            }

            function closeSettings() {
                document.getElementById("settingsModal").classList.add("hidden");
            }

            // ==========================================
            // 身份
            // ==========================================

            function base64url(bytes) {
                let bin = "";
                bytes.forEach((b) => (bin += String.fromCharCode(b)));
                return btoa(bin)
                    .replace(/\+/g, "-")
                    .replace(/\//g, "_")
                    .replace(/=+$/, "");
            }

            function base64urlToBytes(s) {
                const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
                const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
                const bin = atob(padded);
                const bytes = new Uint8Array(bin.length);
                for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
                return bytes;
            }

            async function ensureIdentity() {
                let stored = null;
                try {
                    stored = localStorage.getItem(IDENTITY_KEY);
                } catch {
                    // storage unavailable; fall through to creation
                }
                if (stored) {
                    try {
                        const parsed = JSON.parse(stored);
                        if (
                            parsed.publicKey &&
                            parsed.privateKey &&
                            (await identityMatches(parsed))
                        ) {
                            identity = parsed;
                            return parsed;
                        }
                        toast(t("err_identity_invalid"), "error");
                    } catch {
                        // fall through to createIdentity
                    }
                }
                return createIdentity();
            }

            async function identityMatches(id) {
                try {
                    const key = await crypto.subtle.importKey(
                        "pkcs8",
                        base64urlToBytes(id.privateKey),
                        { name: "Ed25519" },
                        true,
                        ["sign"],
                    );
                    const jwk = await crypto.subtle.exportKey("jwk", key);
                    return jwk.x === id.publicKey;
                } catch {
                    return false;
                }
            }

            async function generateRandomIdentity() {
                const keyPair = await crypto.subtle.generateKey(
                    { name: "Ed25519" },
                    true,
                    ["sign", "verify"],
                );
                const pubRaw = new Uint8Array(
                    await crypto.subtle.exportKey("raw", keyPair.publicKey),
                );
                const privRaw = new Uint8Array(
                    await crypto.subtle.exportKey("pkcs8", keyPair.privateKey),
                );
                return {
                    publicKey: base64url(pubRaw),
                    privateKey: base64url(privRaw),
                };
            }

            function saveIdentity(next) {
                try {
                    localStorage.setItem(IDENTITY_KEY, JSON.stringify(next));
                } catch {
                    toast(t("err_identity_save"), "error");
                    return false;
                }
                // The avatar belongs to the site-scoped public key; switching
                // identities must not leak the previous identity's avatar.
                if (!identity || identity.publicKey !== next.publicKey) {
                    clearOwnAvatarCache();
                }
                identity = next;
                renderIdentity();
                refreshOwnProfile();
                return true;
            }

            function bip39Ready() {
                return !!(window.bip39 && window.bip39Wordlist);
            }

            // @scure/bip39 v2.3 的 generateMnemonic 返回字符串；旧版
            // @metamask fork 可能返回 Uint8Array（Uint16Array 词索引），
            // 这里统一转成空格分隔的字符串。
            function mnemonicToString(mnemonic) {
                if (typeof mnemonic === "string") return mnemonic;
                const words = [];
                const view = new DataView(
                    mnemonic.buffer,
                    mnemonic.byteOffset,
                    mnemonic.byteLength,
                );
                for (let i = 0; i < view.byteLength; i += 2) {
                    words.push(window.bip39Wordlist[view.getUint16(i, true)]);
                }
                return words.join(" ");
            }

            async function hmacSha512(keyBytes, dataBytes) {
                const key = await crypto.subtle.importKey(
                    "raw",
                    keyBytes,
                    { name: "HMAC", hash: "SHA-512" },
                    false,
                    ["sign"],
                );
                return new Uint8Array(
                    await crypto.subtle.sign("HMAC", key, dataBytes),
                );
            }

            function ser32(index) {
                const bytes = new Uint8Array(4);
                new DataView(bytes.buffer).setUint32(0, index >>> 0);
                return bytes;
            }

            async function slip10Master(seed) {
                const i = await hmacSha512(
                    new TextEncoder().encode("ed25519 seed"),
                    seed,
                );
                return { key: i.slice(0, 32), chainCode: i.slice(32) };
            }

            async function slip10Child(node, index) {
                const data = new Uint8Array(1 + 32 + 4);
                data[0] = 0;
                data.set(node.key, 1);
                data.set(ser32(index), 33);
                const i = await hmacSha512(node.chainCode, data);
                return { key: i.slice(0, 32), chainCode: i.slice(32) };
            }

            async function mnemonicToIdentity(mnemonic) {
                const normalized = mnemonic
                    .trim()
                    .toLowerCase()
                    .split(/\s+/)
                    .join(" ");
                if (!bip39Ready()) {
                    throw new Error(t("err_bip39_load"));
                }
                if (
                    !window.bip39.validateMnemonic(
                        normalized,
                        window.bip39Wordlist,
                    )
                ) {
                    throw new Error(t("err_mnemonic_invalid"));
                }
                const seed = window.bip39.mnemonicToSeedSync(normalized);
                let node = await slip10Master(seed);
                // SLIP-0010 m/44'/1328'/0'：Cumments 演示使用的固定派生路径
                // （后端只认最终公钥，不感知派生方式；改路径会让旧助记词失效）
                for (const index of [
                    0x80000000 + 44,
                    0x80000000 + 1328,
                    0x80000000,
                ]) {
                    node = await slip10Child(node, index);
                }
                // 48 字节 PKCS#8 DER：固定前缀 + 32 字节 Ed25519 种子
                const der = new Uint8Array(48);
                der.set(
                    [
                        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03,
                        0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
                    ],
                    0,
                );
                der.set(node.key, 16);
                const privateKey = await crypto.subtle.importKey(
                    "pkcs8",
                    der,
                    { name: "Ed25519" },
                    true,
                    ["sign"],
                );
                const jwk = await crypto.subtle.exportKey("jwk", privateKey);
                return { publicKey: jwk.x, privateKey: base64url(der) };
            }

            async function createIdentity() {
                if (bip39Ready()) {
                    const mnemonic = mnemonicToString(
                        window.bip39.generateMnemonic(window.bip39Wordlist),
                    );
                    try {
                        sessionStorage.setItem(MNEMONIC_SESSION_KEY, mnemonic);
                    } catch {
                        // 隐私模式等场景下 sessionStorage 可能不可用，不影响创建
                    }
                    const next = await mnemonicToIdentity(mnemonic);
                    pendingIdentity = next;
                    showMnemonicModal(mnemonic, "backup");
                    return next;
                }
                const next = await generateRandomIdentity();
                saveIdentity(next);
                toast(
                    t("mnemonic_fallback_toast"),
                    "error",
                );
                return next;
            }

            function showMnemonicModal(mnemonic, mode) {
                mnemonicModalMode = mode;
                currentMnemonic = mnemonic;
                const box = document.getElementById("mnemonicWords");
                box.innerHTML = "";
                mnemonic.split(" ").forEach((word, i) => {
                    const el = document.createElement("div");
                    el.className =
                        "rounded-lg bg-white border border-slate-200 px-2 py-1.5 text-center";
                    el.textContent = `${i + 1}. ${word}`;
                    box.appendChild(el);
                });
                document.getElementById("mnemonicModalTitle").textContent =
                    mode === "backup"
                        ? t("mnemonic_title_backup")
                        : t("mnemonic_title_view");
                document.getElementById("mnemonicModalDesc").textContent =
                    mode === "backup"
                        ? t("mnemonic_desc_backup")
                        : t("mnemonic_desc_view");
                document.getElementById("mnemonicModalPrimary").textContent =
                    mode === "backup" ? t("mnemonic_done") : t("close");
                document.getElementById("mnemonicModalAlt").classList.toggle(
                    "hidden",
                    mode !== "backup",
                );
                document.getElementById("mnemonicModal").classList.remove(
                    "hidden",
                );
            }

            function closeMnemonicModal() {
                document.getElementById("mnemonicModal").classList.add("hidden");
                currentMnemonic = null;
            }

            async function acknowledgeMnemonicBackup() {
                if (mnemonicModalMode !== "backup") {
                    closeMnemonicModal();
                    return;
                }
                const next = pendingIdentity;
                if (!next) {
                    closeMnemonicModal();
                    return;
                }
                const switched =
                    !!identity && identity.publicKey !== next.publicKey;
                if (!saveIdentity(next)) {
                    toast(t("err_try_again"), "error");
                    return;
                }
                pendingIdentity = null;
                closeMnemonicModal();
                toast(t("identity_created"), "success");
                if (switched) await loadList();
            }

            async function discardMnemonicBackup() {
                closeMnemonicModal();
                try {
                    sessionStorage.removeItem(MNEMONIC_SESSION_KEY);
                } catch {
                    // ignore
                }
                const previous = identity;
                pendingIdentity = null;
                let next;
                try {
                    next = await generateRandomIdentity();
                } catch (e) {
                    toast(t("err_random_identity") + (e.message || e), "error");
                    return;
                }
                if (!saveIdentity(next)) return;
                toast(t("identity_random"), "info");
                if (previous && previous.publicKey !== next.publicKey) {
                    await loadList();
                }
            }

            async function copyMnemonic() {
                if (!currentMnemonic) return;
                try {
                    await navigator.clipboard.writeText(currentMnemonic);
                    toast(t("mnemonic_copied"));
                } catch {
                    toast(t("mnemonic_copy_failed"), "error");
                }
            }

            function showMnemonic() {
                let mnemonic = null;
                try {
                    mnemonic = sessionStorage.getItem(MNEMONIC_SESSION_KEY);
                } catch {
                    // ignore
                }
                if (!mnemonic) {
                    toast(t("mnemonic_view_notice"), "info");
                    return;
                }
                showMnemonicModal(mnemonic, "view");
            }

            function toggleMnemonicRestore() {
                const box = document.getElementById("mnemonicRestoreBox");
                box.classList.toggle("hidden");
                if (!box.classList.contains("hidden")) {
                    document.getElementById("mnemonicInput").focus();
                }
            }

            async function restoreFromMnemonic() {
                const input = document
                    .getElementById("mnemonicInput")
                    .value.trim();
                if (!input) {
                    toast(t("err_mnemonic_input"), "error");
                    return;
                }
                let next;
                try {
                    next = await mnemonicToIdentity(input);
                } catch (e) {
                    toast(e.message || t("err_mnemonic_invalid"), "error");
                    return;
                }
                const normalized = input
                    .toLowerCase()
                    .split(/\s+/)
                    .join(" ");
                if (identity && identity.publicKey === next.publicKey) {
                    try {
                        sessionStorage.setItem(
                            MNEMONIC_SESSION_KEY,
                            normalized,
                        );
                    } catch {
                        // ignore
                    }
                    toast(t("mnemonic_same"), "success");
                    return;
                }
                if (
                    !confirm(t("confirm_restore"))
                ) {
                    return;
                }
                if (!saveIdentity(next)) return;
                try {
                    sessionStorage.setItem(MNEMONIC_SESSION_KEY, normalized);
                } catch {
                    // ignore
                }
                document.getElementById("mnemonicRestoreBox").classList.add(
                    "hidden",
                );
                document.getElementById("mnemonicInput").value = "";
                toast(t("mnemonic_restored"), "success");
                await loadList();
            }

            function exportPrivateKey() {
                if (!identity) {
                    toast(t("err_identity_not_ready"), "error");
                    return;
                }
                const payload = {
                    version: 1,
                    publicKey: identity.publicKey,
                    privateKey: identity.privateKey,
                };
                const blob = new Blob([JSON.stringify(payload, null, 2)], {
                    type: "application/json",
                });
                const url = URL.createObjectURL(blob);
                const a = document.createElement("a");
                a.href = url;
                a.download = `cumments-identity-${identity.publicKey.slice(0, 8)}.json`;
                a.click();
                URL.revokeObjectURL(url);
                toast(t("private_key_exported"), "success");
            }

            async function importPrivateKeyFile(event) {
                const file = event.target.files && event.target.files[0];
                event.target.value = "";
                if (!file) return;
                try {
                    const parsed = JSON.parse(await file.text());
                    if (
                        !parsed ||
                        parsed.version !== 1 ||
                        !parsed.publicKey ||
                        !parsed.privateKey
                    ) {
                        throw new Error(t("err_import_format"));
                    }
                    const privateKey = await crypto.subtle.importKey(
                        "pkcs8",
                        base64urlToBytes(parsed.privateKey),
                        { name: "Ed25519" },
                        true,
                        ["sign"],
                    );
                    const jwk = await crypto.subtle.exportKey(
                        "jwk",
                        privateKey,
                    );
                    if (jwk.x !== parsed.publicKey) {
                        throw new Error(t("err_import_mismatch"));
                    }
                    if (
                        identity &&
                        identity.publicKey === parsed.publicKey
                    ) {
                        toast(t("import_current"), "success");
                        return;
                    }
                    if (!confirm(t("confirm_import"))) {
                        return;
                    }
                    const saved = saveIdentity({
                        publicKey: parsed.publicKey,
                        privateKey: parsed.privateKey,
                    });
                    if (!saved) return;
                    try {
                        sessionStorage.removeItem(MNEMONIC_SESSION_KEY);
                    } catch {
                        // ignore
                    }
                    toast(t("private_key_imported"), "success");
                    await loadList();
                } catch (e) {
                    toast(e.message || t("err_import_fail"), "error");
                }
            }

            async function resetIdentity() {
                if (!confirm(t("confirm_reset"))) {
                    return;
                }
                const old = identity;
                await createIdentity();
                // 助记词模式：等用户确认备份后才切换身份并刷新列表；
                // CDN 不可用降级为随机身份时立即切换，这里直接刷新。
                if (
                    !pendingIdentity &&
                    old &&
                    identity &&
                    identity.publicKey !== old.publicKey
                ) {
                    await loadList();
                }
            }

            function renderIdentity() {
                const el = document.getElementById("publicKey");
                if (el) el.value = identity ? identity.publicKey : "";
                renderSettingsAvatar();
                updateComposerAvatar();
            }

            async function copyPublicKey() {
                if (!identity) return;
                try {
                    await navigator.clipboard.writeText(identity.publicKey);
                    toast(t("public_key_copied"));
                } catch {
                    const el = document.getElementById("publicKey");
                    el.select();
                    document.execCommand("copy");
                    toast(t("public_key_copied"));
                }
            }

            async function importPrivateKey(privateKeyB64) {
                return crypto.subtle.importKey(
                    "pkcs8",
                    base64urlToBytes(privateKeyB64),
                    { name: "Ed25519" },
                    false,
                    ["sign"],
                );
            }

            async function signMessage(privateKeyB64, message) {
                const key = await importPrivateKey(privateKeyB64);
                const sig = await crypto.subtle.sign(
                    { name: "Ed25519" },
                    key,
                    new TextEncoder().encode(message),
                );
                return base64url(new Uint8Array(sig));
            }

            async function authorSignature(parts) {
                const id = identity || (await ensureIdentity());
                const signature = await signMessage(
                    id.privateKey,
                    parts.join("\n"),
                );
                return { publicKey: id.publicKey, signature };
            }

            function isOwn(comment) {
                return (
                    !!identity &&
                    comment.author &&
                    comment.author.type === "guest" &&
                    comment.author.public_key === identity.publicKey
                );
            }

            // Once the comment we just submitted appears in the read model
            // (via SSE or a list refresh), move the composer status from
            // "waiting for Matrix sync" to "synced".
            function markPendingSynced(comments) {
                const pending = state.pendingComment;
                if (!pending) return;
                const synced = comments.some((c) => {
                    if (
                        pending.submissionId != null &&
                        c.submission_id === pending.submissionId
                    ) {
                        return true;
                    }
                    if (
                        !c.author ||
                        c.author.type !== "guest" ||
                        c.author.public_key !== pending.publicKey ||
                        signableContent(c.content) !== pending.content
                    ) {
                        return false;
                    }
                    const ts = c.timestamp ? Date.parse(c.timestamp) : NaN;
                    return Number.isFinite(ts) && ts >= pending.submittedAt;
                });
                if (synced) {
                    state.pendingComment = null;
                    stopPendingSyncPoll();
                    const status = document.getElementById("powStatus");
                    if (status) status.textContent = t("status_synced");
                    toast(t("comment_synced"));
                }
            }

            // SSE can be temporarily unavailable; poll a few times so the
            // composer status never stays stuck on "waiting for Matrix sync".
            function startPendingSyncPoll() {
                stopPendingSyncPoll();
                let attempts = 0;
                const poll = () => {
                    if (!state.pendingComment) return;
                    attempts += 1;
                    loadList();
                    if (attempts >= PENDING_POLL_LIMIT) {
                        const status = document.getElementById("powStatus");
                        if (status) {
                            status.textContent = t("status_still_waiting");
                        }
                        pendingPollTimer = setTimeout(
                            poll,
                            PENDING_POLL_LONG_INTERVAL_MS,
                        );
                    } else {
                        pendingPollTimer = setTimeout(poll, PENDING_POLL_INTERVAL_MS);
                    }
                };
                pendingPollTimer = setTimeout(poll, PENDING_POLL_INTERVAL_MS);
            }

            function stopPendingSyncPoll() {
                if (pendingPollTimer !== null) {
                    clearTimeout(pendingPollTimer);
                    pendingPollTimer = null;
                }
            }

            function authorName(comment) {
                const author = comment.author || {};
                if (author.type === "matrix") {
                    return author.mxid
                        ? author.mxid.replace(/^@/, "").split(":")[0]
                        : t("matrix_user");
                }
                return author.display_name || t("guest_default");
            }

            function authorAvatarKey(comment) {
                const author = comment.author || {};
                return author.public_key || author.mxid || comment.event_id;
            }

            // Stable, short, deterministic tag for a guest identity. The
            // display name may change freely; the public key never does, so
            // this tag is how the UI shows "same person, different name".
            function guestIdentityTag(publicKey) {
                let hash = 0;
                for (const ch of publicKey) {
                    hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
                }
                return "#" + hash.toString(16).padStart(6, "0").slice(0, 6);
            }

            function mxidShort(mxid) {
                return mxid.replace(/^@/, "").split(":")[0];
            }

            function governanceBadge(comment) {
                const author = comment.author || {};
                if (author.type !== "matrix" || !author.mxid) return "";
                const mxid = author.mxid;
                if (state.siteOwners.has(mxid)) {
                    return `<span class="text-[10px] font-medium text-amber-700 bg-amber-50 rounded px-1.5 py-0.5">${t("role_owner")}</span>`;
                }
                if (state.siteCoManagers.has(mxid)) {
                    return `<span class="text-[10px] font-medium text-purple-700 bg-purple-50 rounded px-1.5 py-0.5">${t("role_co_manager")}</span>`;
                }
                if (state.roomModerators.has(mxid)) {
                    return `<span class="text-[10px] font-medium text-emerald-700 bg-emerald-50 rounded px-1.5 py-0.5">${t("role_moderator")}</span>`;
                }
                return "";
            }

            // ==========================================
            // API
            // ==========================================

            async function apiError(res) {
                let message = res.statusText || `HTTP ${res.status}`;
                try {
                    const body = await res.json();
                    if (body.detail) message = body.detail;
                    else if (body.title) message = body.title;
                    if (body.code) message += ` (${body.code})`;
                } catch {
                    // keep status text
                }
                return message;
            }

            async function queryComments(cfg, page, perPage) {
                const res = await fetch(
                    `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/comments`,
                    {
                        method: "QUERY",
                        headers: { "Content-Type": "application/json" },
                        body: JSON.stringify({ page, per_page: perPage }),
                    },
                );
                if (!res.ok) throw new Error(await apiError(res));
                return res.json();
            }

            async function getChallenge(cfg) {
                const res = await fetch(`${cfg.api}/api/v1/challenge`);
                if (!res.ok) throw new Error(await apiError(res));
                return res.json();
            }

            // One key per logical write; a retry of the same request should
            // reuse the same key so the server replays the original intent.
            function newIdempotencyKey() {
                if (crypto.randomUUID) return crypto.randomUUID();
                const bytes = crypto.getRandomValues(new Uint8Array(16));
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                const hex = Array.from(bytes, (b) =>
                    b.toString(16).padStart(2, "0"),
                ).join("");
                return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(
                    12,
                    16,
                )}-${hex.slice(16, 20)}-${hex.slice(20)}`;
            }

            // ==========================================
            // 评论列表
            // ==========================================

            function showLoading() {
                const container = document.getElementById("commentsContainer");
                container.innerHTML = `
                    <div class="text-center py-10 text-slate-400 bg-white rounded-2xl border border-dashed border-slate-200">
                        ${t("loading")}
                    </div>
                `;
            }

            function renderEmpty(message) {
                const container = document.getElementById("commentsContainer");
                container.innerHTML = `
                    <div class="text-center py-10 text-slate-400 bg-white rounded-2xl border border-dashed border-slate-200">
                        ${message}
                    </div>
                `;
            }

            function renderError(e) {
                const container = document.getElementById("commentsContainer");
                container.innerHTML = `
                    <div class="text-center py-8 text-red-500 bg-red-50 rounded-2xl border border-red-100">
                        ${escapeHtml(t("load_failed") + e.message)}
                    </div>
                `;
            }

            async function loadList() {
                try {
                    if (state.filter === "all") {
                        await loadAll();
                    } else {
                        await loadMine();
                    }
                } catch (e) {
                    renderError(e);
                }
            }

            async function loadAll() {
                const cfg = getSettings();
                const json = await queryComments(cfg, state.currentPage, state.perPage);
                state.allComments = json.data;
                state.meta = json.meta;
                state.mineTotal = 0;
                renderComments(json.data);
                markPendingSynced(json.data);
                updateHeader(json.meta.total);
                updatePagination(json.meta);
            }

            async function loadMine() {
                if (!state.meta) await loadAll();
                const cfg = getSettings();
                const collected = [];
                let mineCount = 0;
                let backendPage = 1;
                const maxPage = state.meta ? state.meta.total_pages : 0;
                let page = state.currentPage;
                const need = page * state.perPage;

                while (backendPage <= maxPage) {
                    const json = await queryComments(cfg, backendPage, 100);
                    for (const c of json.data) {
                        if (!isOwn(c)) continue;
                        mineCount += 1;
                        if (collected.length < need) collected.push(c);
                    }
                    if (backendPage >= (json.meta ? json.meta.total_pages : backendPage)) {
                        break;
                    }
                    backendPage += 1;
                }

                state.mineTotal = mineCount;
                const totalPages = Math.max(
                    1,
                    Math.ceil(mineCount / state.perPage),
                );
                if (page > totalPages) {
                    page = totalPages;
                    state.currentPage = page;
                }
                const start = (page - 1) * state.perPage;
                const items = collected.slice(start, start + state.perPage);
                state.mineComments = items;

                const meta = {
                    total: mineCount,
                    page,
                    per_page: state.perPage,
                    total_pages: totalPages,
                };
                renderComments(items);
                markPendingSynced(items);
                loadRoomInfo();
                updateHeader(mineCount);
                updatePagination(meta);
            }

            function renderComments(items) {
                refreshOwnAvatarFromComments(items);
                const container = document.getElementById("commentsContainer");
                container.innerHTML = "";
                if (!items.length) {
                    renderEmpty(
                        state.filter === "mine"
                            ? t("empty_mine")
                            : t("empty_all"),
                    );
                    return;
                }

                const { roots, byParent } = buildReplyTree(items);
                roots.forEach((comment) => {
                    container.appendChild(renderCommentBranch(comment, byParent, 0));
                });
            }

            async function loadRoomInfo() {
                const cfg = getSettings();
                try {
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/room`,
                    );
                    if (!res.ok) return;
                    const info = await res.json();
                    renderRoomInfo(info);
                } catch {
                    // Room info is decorative; ignore failures.
                }
            }

            async function loadRoles() {
                const cfg = getSettings();
                try {
                    const [siteRes, roomRes] = await Promise.all([
                        fetch(`${cfg.api}/api/v1/sites/${cfg.siteId}/roles`),
                        fetch(
                            `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/moderators`,
                        ),
                    ]);
                    if (siteRes.ok) {
                        const roles = await siteRes.json();
                        state.siteOwners = new Set(roles.owners || []);
                        state.siteCoManagers = new Set(roles.co_managers || []);
                    }
                    if (roomRes.ok) {
                        const moderators = await roomRes.json();
                        state.roomModerators = new Set(moderators.moderators || []);
                    }
                } catch {
                    // Governance badges are decorative; ignore failures.
                }
            }

            function renderRoomInfo(info) {
                let header = document.getElementById("roomHeader");
                if (!header) {
                    header = document.createElement("div");
                    header.id = "roomHeader";
                    const container = document.getElementById("commentsContainer");
                    container.parentNode.insertBefore(header, container);
                }
                const cfg = getSettings();
                const roomAvatar = apiMediaUrl(
                    info.avatar_thumbnail_url || info.avatar_url,
                );
                const avatar = roomAvatar
                    ? `<img src="${escapeHtml(roomAvatar)}" alt="" class="w-9 h-9 rounded-full object-cover shrink-0">`
                    : `<div class="w-9 h-9 rounded-full bg-indigo-100 text-indigo-600 flex items-center justify-center font-bold shrink-0">💬</div>`;
                const name = info.name
                    ? escapeHtml(info.name)
                    : escapeHtml(cfg.slug);
                const joinedUsers = new Map();
                const system = (info.system_messages || [])
                    .slice()
                    .reverse()
                    .map((event) => renderSystemMessage(event, joinedUsers))
                    .filter(Boolean)
                    .join("");
                const governanceUsers = [
                    ...new Set([
                        ...state.siteOwners,
                        ...state.siteCoManagers,
                        ...state.roomModerators,
                    ]),
                ]
                    .map(mxidShort)
                    .join(", ");
                const governanceLine = governanceUsers
                    ? `<div class="text-xs text-slate-400 mt-0.5">${escapeHtml(t("room_roles"))}: ${escapeHtml(governanceUsers)}</div>`
                    : "";
                header.innerHTML = `
                    <div class="flex items-center gap-3 bg-white rounded-2xl border border-slate-200 shadow-sm p-4 mb-4">
                        ${avatar}
                        <div class="min-w-0">
                            <div class="font-semibold text-slate-900 text-sm truncate">${name}</div>
                            <div id="roomMemberInfo" class="text-xs text-slate-400">${info.member_count ?? 0} ${t("members")}</div>
                            ${governanceLine}
                        </div>
                    </div>
                    ${system ? `<div class="space-y-1 mb-4">${system}</div>` : ""}
                `;
                const memberInfo = header.querySelector("#roomMemberInfo");
                if (memberInfo) {
                    memberInfo.dataset.memberCount = String(info.member_count ?? 0);
                }
                updatePresenceIndicator();
            }

            function renderSystemMessage(event, joinedUsers) {
                const content = event.content_json || {};
                const time = formatTime(new Date(event.origin_server_ts).toISOString());
                let text = "";
                switch (event.event_type) {
                    case "m.room.member": {
                        const user = event.state_key || "";
                        const name = content.displayname || user;
                        if (content.membership === "join") {
                            if (joinedUsers.has(user)) {
                                // A join while already joined is a display-name
                                // update (homeserver profile propagation), not
                                // a new member. Skip identical re-joins.
                                text =
                                    joinedUsers.get(user) === name
                                        ? ""
                                        : `${name} ${t("member_renamed")}`;
                            } else {
                                text = `${name} ${t("member_joined")}`;
                            }
                            joinedUsers.set(user, name);
                        } else if (content.membership === "leave") {
                            text = `${name} ${t("member_left")}`;
                            joinedUsers.delete(user);
                        } else {
                            return "";
                        }
                        break;
                    }
                    case "m.room.name":
                        text = t("room_name_changed") + (content.name || "");
                        break;
                    case "m.room.topic":
                        text = t("room_topic_changed") + (content.topic || "");
                        break;
                    case "m.room.avatar":
                        text = t("room_avatar_changed");
                        break;
                    default:
                        return "";
                }
                return `<div class="text-xs text-slate-400 px-1"><span class="text-slate-500">${escapeHtml(text)}</span> · ${time}</div>`;
            }

            const MAX_REPLY_DEPTH = 8;

            function buildReplyTree(items) {
                const byParent = new Map();
                const ids = new Set(items.map((c) => c.event_id));
                const roots = [];

                for (const comment of items) {
                    if (comment.reply_to && ids.has(comment.reply_to)) {
                        if (!byParent.has(comment.reply_to)) {
                            byParent.set(comment.reply_to, []);
                        }
                        byParent.get(comment.reply_to).push(comment);
                    } else {
                        roots.push(comment);
                    }
                }

                const byTime = (a, b) => new Date(a.timestamp) - new Date(b.timestamp);
                roots.sort((a, b) => byTime(b, a));
                for (const children of byParent.values()) {
                    children.sort(byTime);
                }

                return { roots, byParent };
            }

            function renderCommentBranch(comment, byParent, depth) {
                const wrapper = document.createElement("div");
                wrapper.appendChild(createCommentElement(comment));

                const children = byParent.get(comment.event_id);
                if (children && children.length) {
                    if (depth < MAX_REPLY_DEPTH) {
                        const childBox = document.createElement("div");
                        childBox.className =
                            "ml-6 pl-4 border-l-2 border-slate-100 space-y-4 mt-4";
                        children.forEach((child) => {
                            childBox.appendChild(
                                renderCommentBranch(child, byParent, depth + 1),
                            );
                        });
                        wrapper.appendChild(childBox);
                    } else {
                        const note = document.createElement("div");
                        note.className =
                            "ml-6 pl-4 text-xs text-slate-400 border-l-2 border-slate-100 mt-3";
                        note.textContent = t("depth_limit");
                        wrapper.appendChild(note);
                    }
                }

                return wrapper;
            }

            function createCommentElement(comment) {
                const own = isOwn(comment);
                const el = document.createElement("article");
                el.dataset.id = comment.event_id;
                el.className =
                    "bg-white rounded-2xl border border-slate-200 shadow-sm p-5 hover:shadow-md transition group";

                const avatarStyle = commentAvatarStyle(authorAvatarKey(comment));
                const initials = authorName(comment)[0].toUpperCase();
                const authorAvatarUrl =
                    comment.author && comment.author.avatar_url
                        ? apiMediaUrl(comment.author.avatar_url)
                        : null;
                const time = formatTime(comment.timestamp);
                const edited = comment.edited_at ? ` · ${t("edited")}` : "";
                const isGuest =
                    comment.author && comment.author.type === "guest";
                const badge = isGuest
                    ? `<span class="text-[10px] font-medium text-slate-500 bg-slate-100 rounded px-1.5 py-0.5">${t("guest_badge")}</span>`
                    : `<span class="text-[10px] font-medium text-indigo-600 bg-indigo-50 rounded px-1.5 py-0.5">Matrix</span>`;
                const identityTag =
                    isGuest && comment.author.public_key
                        ? `<span class="text-[10px] font-mono text-slate-400" title="${escapeHtml(t("identity_tag_title"))}">${escapeHtml(guestIdentityTag(comment.author.public_key))}</span>`
                        : "";

                el.innerHTML = `
                    <div class="flex items-start justify-between gap-3">
                        <div class="flex items-center gap-3 min-w-0">
                            ${authorAvatarUrl
                                ? `<img src="${escapeHtml(authorAvatarUrl)}" alt="" data-avatar class="w-9 h-9 rounded-full object-cover shrink-0" loading="lazy">`
                                : `<div class="w-9 h-9 rounded-full shrink-0 flex items-center justify-center text-sm font-bold text-white"
                                     style="${avatarStyle}">
                                    ${escapeHtml(initials)}
                                </div>`}
                            <div class="min-w-0">
                                <div class="flex items-center gap-2 flex-wrap">
                                    <span class="font-semibold text-slate-900 text-sm truncate">
                                        ${escapeHtml(authorName(comment))}
                                    </span>
                                    ${badge}
                                    ${identityTag}
                                    ${governanceBadge(comment)}
                                </div>
                                <div class="text-xs text-slate-400 mt-0.5">${time}${edited}</div>
                            </div>
                        </div>
                        <div class="flex gap-1 opacity-0 group-hover:opacity-100 transition">
                            <button class="reply-btn text-xs text-slate-500 hover:text-indigo-600 px-1.5 py-1">${t("reply")}</button>
                            ${own ? `
                            <button class="edit-btn text-xs text-slate-500 hover:text-indigo-600 px-1.5 py-1">${t("edit")}</button>
                            <button class="delete-btn text-xs text-slate-500 hover:text-red-600 px-1.5 py-1">${t("delete")}</button>
                            ` : ""}
                        </div>
                    </div>
                    <div class="comment-content markdown-body text-sm text-slate-700 leading-relaxed break-words mt-3 pl-12">
                        ${comment.status === "redacted"
                            ? `<span class="italic text-slate-400">${t("deleted_message")}</span>`
                            : renderContent(comment.content)}
                    </div>
                    ${comment.reactions && comment.reactions.length
                        ? `<div class="flex gap-1.5 flex-wrap mt-2 pl-12">${renderReactions(comment.reactions)}</div>`
                        : ""}
                    <div class="mt-1 pl-12">
                        <button type="button" class="react-btn text-xs text-slate-400 hover:text-indigo-600 px-1 py-0.5">＋</button>
                    </div>
                    <div class="edit-box hidden mt-3 pl-12">
                        <textarea class="edit-textarea w-full text-sm border border-slate-200 rounded-lg p-2.5 focus:border-indigo-400 focus:ring-2 focus:ring-indigo-100 outline-none resize-y"
                                  rows="3"></textarea>
                        <div class="flex gap-2 justify-end mt-2">
                            <button class="cancel-edit-btn text-xs text-slate-500 hover:text-slate-700 px-2 py-1">${t("cancel")}</button>
                            <button class="save-edit-btn text-xs bg-green-600 hover:bg-green-700 text-white rounded-md px-3 py-1.5 font-medium">${t("save_edit")}</button>
                        </div>
                    </div>
                `;

                const avatarImg = el.querySelector("img[data-avatar]");
                if (avatarImg) {
                    // 签名代理 URL 可能过期或媒体已被删除：加载失败时回退到
                    // 原来的首字母色块，不破坏布局。
                    avatarImg.addEventListener("error", () => {
                        const fallback = document.createElement("div");
                        fallback.className =
                            "w-9 h-9 rounded-full shrink-0 flex items-center justify-center text-sm font-bold text-white";
                        fallback.style.cssText = avatarStyle;
                        fallback.textContent = initials;
                        avatarImg.replaceWith(fallback);
                    });
                }
                el.querySelector(".reply-btn").onclick = () => startReply(comment);
                el.querySelector(".react-btn").onclick = () => pickReaction(comment);
                el.querySelectorAll(".poll-option").forEach((btn) => {
                    btn.onclick = () => submitVote(comment.event_id, btn.dataset.option);
                });
                if (own) {
                    bindEdit(el, comment);
                    el.querySelector(".delete-btn").onclick = () =>
                        deleteComment(comment.event_id);
                }

                return el;
            }

            function bindEdit(el, comment) {
                const editBtn = el.querySelector(".edit-btn");
                const cancelBtn = el.querySelector(".cancel-edit-btn");
                const saveBtn = el.querySelector(".save-edit-btn");
                const editBox = el.querySelector(".edit-box");
                const contentBox = el.querySelector(".comment-content");
                const textarea = el.querySelector(".edit-textarea");

                editBtn.onclick = () => {
                    textarea.value = textBody(comment.content);
                    editBox.classList.remove("hidden");
                    contentBox.classList.add("hidden");
                    textarea.focus();
                };

                cancelBtn.onclick = () => {
                    editBox.classList.add("hidden");
                    contentBox.classList.remove("hidden");
                };

                saveBtn.onclick = async () => {
                    const next = textarea.value.trim();
                    if (!next) {
                        toast(t("err_content_empty_edit"), "error");
                        return;
                    }
                    try {
                        await submitEdit(comment, next);
                        comment.content = {
                            type: "text",
                            body: next,
                            style: "normal",
                        };
                        contentBox.innerHTML = renderContent(comment.content);
                        editBox.classList.add("hidden");
                        contentBox.classList.remove("hidden");
                        toast(t("comment_updated"));
                    } catch (e) {
                        toast(t("edit_failed") + e.message, "error");
                    }
                };
            }

            function updateHeader(count) {
                document.getElementById("commentCount").textContent = count || 0;
            }

            function updatePagination(meta) {
                const info = document.getElementById("pageInfo");
                const start = meta.total > 0 ? (meta.page - 1) * meta.per_page + 1 : 0;
                const end = meta.total > 0 ? Math.min(start + meta.per_page - 1, meta.total) : 0;
                info.textContent =
                    meta.total > 0
                        ? t("page_info", { start, end, total: meta.total })
                        : t("no_data");
                document.getElementById("prevBtn").disabled = meta.page <= 1;
                document.getElementById("nextBtn").disabled =
                    meta.page >= meta.total_pages;
            }

            function changePage(delta) {
                const next = state.currentPage + delta;
                const max =
                    state.filter === "mine"
                        ? Math.max(1, Math.ceil(state.mineTotal / state.perPage))
                        : state.meta
                          ? state.meta.total_pages
                          : 1;
                if (next < 1 || next > max) return;
                state.currentPage = next;
                showLoading();
                loadList();
            }

            function switchTab(filter) {
                if (state.filter === filter) return;
                state.filter = filter;
                state.currentPage = 1;
                const allBtn = document.getElementById("tabAll");
                const mineBtn = document.getElementById("tabMine");
                if (filter === "all") {
                    allBtn.className =
                        "px-3 py-1.5 rounded-md font-medium text-slate-700 bg-white shadow-sm";
                    mineBtn.className =
                        "px-3 py-1.5 rounded-md font-medium text-slate-500 hover:text-slate-700 transition";
                } else {
                    mineBtn.className =
                        "px-3 py-1.5 rounded-md font-medium text-slate-700 bg-white shadow-sm";
                    allBtn.className =
                        "px-3 py-1.5 rounded-md font-medium text-slate-500 hover:text-slate-700 transition";
                }
                showLoading();
                loadList();
            }

            // ==========================================
            // 发布 / 编辑 / 删除
            // ==========================================

            function startReply(comment) {
                state.replyingTo = comment;
                updateReplyBanner();
                document.getElementById("composerContent").focus();
            }

            function cancelReply() {
                state.replyingTo = null;
                updateReplyBanner();
            }

            function updateReplyBanner() {
                const banner = document.getElementById("replyBanner");
                const name = document.getElementById("replyTargetName");
                if (state.replyingTo) {
                    name.textContent =
                        authorName(state.replyingTo);
                    banner.classList.remove("hidden");
                } else {
                    banner.classList.add("hidden");
                }
            }

            function signableContent(content) {
                if (!content || typeof content !== "object") return null;
                if (content.type === "text") return content.body || null;
                if (content.type === "media") return content.url || null;
                return null;
            }

            let mediaRecorder = null;
            let mediaRecorderChunks = [];

            function pickImage() {
                document.getElementById("imageInput").click();
            }

            function toggleVoiceRecord() {
                if (mediaRecorder && mediaRecorder.state === "recording") {
                    mediaRecorder.stop();
                    return;
                }
                if (!navigator.mediaDevices || !window.MediaRecorder) {
                    toast(t("upload_media_failed") + "unsupported", "error");
                    return;
                }
                navigator.mediaDevices
                    .getUserMedia({ audio: true })
                    .then((stream) => {
                        mediaRecorder = new MediaRecorder(stream);
                        mediaRecorderChunks = [];
                        mediaRecorder.ondataavailable = (event) => {
                            if (event.data.size > 0) {
                                mediaRecorderChunks.push(event.data);
                            }
                        };
                        mediaRecorder.onstop = async () => {
                            stream.getTracks().forEach((track) => track.stop());
                            const mime =
                                mediaRecorder.mimeType ||
                                "audio/webm";
                            const blob = new Blob(mediaRecorderChunks, {
                                type: mime,
                            });
                            mediaRecorder = null;
                            document.getElementById("voiceBtn").querySelector("span").textContent = t("record_voice");
                            if (blob.size > 0) {
                                await attachMedia(
                                    blob,
                                    `voice-${Date.now()}.webm`,
                                    mime,
                                    true,
                                );
                            }
                        };
                        mediaRecorder.start();
                        document.getElementById("voiceBtn").querySelector("span").textContent = t("stop_record");
                    })
                    .catch((e) =>
                        toast(t("upload_media_failed") + e.message, "error"),
                    );
            }

            async function attachMedia(blob, filename, mime, voice) {
                try {
                    const cfg = getSettings();
                    const chal = await getChallenge(cfg);
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    const contentHash = await sha256Hex(await blob.arrayBuffer());
                    const { publicKey, signature } = await authorSignature([
                        "UPLOAD",
                        cfg.siteId,
                        cfg.slug,
                        mime,
                        filename,
                        contentHash,
                        chal.prefix,
                    ]);
                    const params = new URLSearchParams({
                        author_public_key: publicKey,
                        author_signature: signature,
                        challenge_response: `${chal.prefix}|${nonce}`,
                        mime,
                        filename,
                    });
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/media?${params.toString()}`,
                        {
                            method: "POST",
                            headers: {
                                "Content-Type": mime,
                                "Idempotency-Key": newIdempotencyKey(),
                            },
                            body: blob,
                        },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    const media = await res.json();
                    state.draftMedia = { ...media, voice };
                    renderMediaPreview(state.draftMedia);
                } catch (e) {
                    toast(t("upload_media_failed") + e.message, "error");
                }
            }

            function renderMediaPreview(media) {
                const el = document.getElementById("mediaPreview");
                if (!media) {
                    el.textContent = "";
                    return;
                }
                const label = media.voice
                    ? t("record_voice")
                    : media.kind === "sticker"
                      ? t("stickers")
                      : media.filename || media.url;
                el.innerHTML = `${escapeHtml(label)} <button type="button" class="text-red-500 hover:text-red-700" title="remove">✕</button>`;
                el.querySelector("button").onclick = () => {
                    state.draftMedia = null;
                    renderMediaPreview(null);
                };
            }

            async function loadStickers() {
                const cfg = getSettings();
                try {
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/stickers`,
                    );
                    if (!res.ok) return;
                    const data = await res.json();
                    state.stickers = (data.packs || []).flatMap((pack) =>
                        (pack.images || []).map((image) => ({
                            ...image,
                            alt: image.body || image.shortcode,
                        })),
                    );
                } catch {
                    state.stickers = [];
                }
            }

            function pickSticker() {
                const stickers = state.stickers || [];
                if (!stickers.length) {
                    toast(t("no_stickers"), "info");
                    return;
                }
                const overlay = document.createElement("div");
                overlay.className =
                    "fixed inset-0 bg-black/40 flex items-center justify-center z-50";
                overlay.innerHTML = `
                    <div class="bg-white rounded-2xl p-4 max-w-md w-full mx-4 shadow-xl">
                        <div class="flex justify-between items-center mb-3">
                            <span class="font-semibold text-sm">${t("stickers")}</span>
                            <button type="button" class="text-slate-400 hover:text-slate-600 text-lg leading-none">×</button>
                        </div>
                        <div class="grid grid-cols-4 gap-2 max-h-72 overflow-y-auto">
                            ${stickers
                                .map(
                                    (sticker) =>
                                        `<button type="button" class="sticker-option rounded-lg hover:bg-slate-100 p-1"><img src="${escapeHtml(apiMediaUrl(sticker.proxy_url))}" alt="${escapeHtml(sticker.alt)}" class="w-full rounded-lg" loading="lazy"></button>`,
                                )
                                .join("")}
                        </div>
                    </div>
                `;
                overlay.addEventListener("click", (event) => {
                    if (event.target === overlay) overlay.remove();
                });
                overlay.querySelector("button").onclick = () => overlay.remove();
                overlay.querySelectorAll(".sticker-option").forEach((btn, index) => {
                    btn.onclick = () => {
                        const sticker = stickers[index];
                        state.draftMedia = {
                            url: sticker.url,
                            filename: sticker.alt,
                            mimetype: sticker.info?.mimetype ?? null,
                            size: sticker.info?.size ?? null,
                            width: sticker.info?.w ?? null,
                            height: sticker.info?.h ?? null,
                            voice: false,
                            kind: "sticker",
                        };
                        renderMediaPreview(state.draftMedia);
                        overlay.remove();
                    };
                });
                document.body.appendChild(overlay);
            }

            const REACTION_EMOJIS = ["👍", "❤️", "😂", "🎉", "🔥"];

            function pickReaction(comment) {
                const overlay = document.createElement("div");
                overlay.className =
                    "fixed inset-0 bg-black/40 flex items-center justify-center z-50";
                overlay.innerHTML = `
                    <div class="bg-white rounded-2xl p-4 shadow-xl">
                        <div class="flex gap-2">
                            ${REACTION_EMOJIS.map(
                                (emoji) =>
                                    `<button type="button" class="reaction-option text-2xl hover:bg-slate-100 rounded-lg p-2">${emoji}</button>`,
                            ).join("")}
                        </div>
                    </div>
                `;
                overlay.addEventListener("click", (event) => {
                    if (event.target === overlay) overlay.remove();
                });
                overlay.querySelectorAll(".reaction-option").forEach((btn, index) => {
                    btn.onclick = async () => {
                        overlay.remove();
                        await submitReaction(comment, REACTION_EMOJIS[index]);
                    };
                });
                document.body.appendChild(overlay);
            }

            async function submitReaction(comment, key) {
                const cfg = getSettings();
                try {
                    const chal = await getChallenge(cfg);
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    const { publicKey, signature } = await authorSignature([
                        "REACT",
                        cfg.siteId,
                        cfg.slug,
                        comment.event_id,
                        key,
                        chal.prefix,
                    ]);
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/comments/${encodeURIComponent(comment.event_id)}/reactions`,
                        {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            body: JSON.stringify({
                                key,
                                author_public_key: publicKey,
                                author_signature: signature,
                                challenge_response: `${chal.prefix}|${nonce}`,
                            }),
                        },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    toast(t("reaction_submitted"), "success");
                    loadList();
                } catch (e) {
                    toast(t("publish_failed") + e.message, "error");
                }
            }

            async function submitVote(pollId, optionId) {
                const cfg = getSettings();
                try {
                    const chal = await getChallenge(cfg);
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    const { publicKey, signature } = await authorSignature([
                        "VOTE",
                        cfg.siteId,
                        cfg.slug,
                        pollId,
                        optionId,
                        chal.prefix,
                    ]);
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/polls/${encodeURIComponent(pollId)}/votes`,
                        {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            body: JSON.stringify({
                                option_id: optionId,
                                author_public_key: publicKey,
                                author_signature: signature,
                                challenge_response: `${chal.prefix}|${nonce}`,
                            }),
                        },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    toast(t("vote_submitted"), "success");
                    loadList();
                } catch (e) {
                    toast(t("publish_failed") + e.message, "error");
                }
            }

            async function sendLocation() {
                if (!navigator.geolocation) {
                    toast(t("publish_failed") + "geolocation unsupported", "error");
                    return;
                }
                state.pendingComment = null;
                stopPendingSyncPoll();
                navigator.geolocation.getCurrentPosition(
                    async (position) => {
                        const geoUri = `geo:${position.coords.latitude},${position.coords.longitude}`;
                        const cfg = getSettings();
                        const displayName =
                            document.getElementById("composerDisplayName").value.trim() ||
                            t("guest_default");
                        try {
                            const chal = await getChallenge(cfg);
                            const nonce = await solvePow(chal.prefix, chal.difficulty);
                            const { publicKey, signature } = await authorSignature([
                                "LOCATE",
                                cfg.siteId,
                                cfg.slug,
                                geoUri,
                                chal.prefix,
                            ]);
                            const res = await fetch(
                                `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/location`,
                                {
                                    method: "POST",
                                    headers: {
                                        "Content-Type": "application/json",
                                        "Idempotency-Key": newIdempotencyKey(),
                                    },
                                    body: JSON.stringify({
                                        geo_uri: geoUri,
                                        description: t("location"),
                                        display_name: displayName,
                                        author_public_key: publicKey,
                                        author_signature: signature,
                                        challenge_response: `${chal.prefix}|${nonce}`,
                                    }),
                                },
                            );
                            if (!res.ok) throw new Error(await apiError(res));
                            let submissionId = null;
                            try {
                                const accepted = await res.json();
                                submissionId = accepted && accepted.submission_id
                                    ? accepted.submission_id
                                    : null;
                            } catch {
                                // Older servers may return a plain 204; fall
                                // back to public-key/content matching.
                            }
                            state.pendingComment = {
                                submissionId,
                                publicKey,
                                content: geoUri,
                                submittedAt: Date.now(),
                            };
                            toast(t("location_submitted"), "success");
                            setTimeout(() => loadList(), 1200);
                            startPendingSyncPoll();
                        } catch (e) {
                            toast(t("publish_failed") + e.message, "error");
                        }
                    },
                    (error) =>
                        toast(t("publish_failed") + error.message, "error"),
                );
            }

            async function submitComment() {
                const cfg = getSettings();
                const displayName =
                    document.getElementById("composerDisplayName").value.trim() ||
                    t("guest_default");
                const content = document
                    .getElementById("composerContent")
                    .value.trim();
                if (!content && !state.draftMedia) {
                    toast(t("err_content_empty"), "error");
                    return;
                }
                const signedContent = state.draftMedia
                    ? state.draftMedia.url
                    : content;
                const replyTo = state.replyingTo
                    ? state.replyingTo.event_id
                    : "";

                const btn = document.getElementById("submitBtn");
                const status = document.getElementById("powStatus");
                btn.disabled = true;
                state.pendingComment = null;
                stopPendingSyncPoll();

                try {
                    status.textContent = t("status_fetch_challenge");
                    const chal = await getChallenge(cfg);
                    status.textContent = t("status_computing_pow", {
                        difficulty: chal.difficulty,
                    });
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    status.textContent = t("status_signing");
                    const { publicKey, signature } = await authorSignature([
                        "POST",
                        cfg.siteId,
                        cfg.slug,
                        signedContent,
                        replyTo,
                        chal.prefix,
                    ]);

                    status.textContent = t("status_submitting");
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/comments`,
                        {
                            method: "POST",
                            headers: {
                                "Content-Type": "application/json",
                                "Idempotency-Key": newIdempotencyKey(),
                            },
                            body: JSON.stringify({
                                content,
                                media: state.draftMedia || null,
                                display_name: displayName,
                                author_public_key: publicKey,
                                author_signature: signature,
                                reply_to: state.replyingTo
                                    ? state.replyingTo.event_id
                                    : null,
                                challenge_response: `${chal.prefix}|${nonce}`,
                            }),
                        },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    let submissionId = null;
                    try {
                        const accepted = await res.json();
                        submissionId = accepted && accepted.submission_id
                            ? accepted.submission_id
                            : null;
                    } catch {
                        // Older servers may return a plain 202 body; fall back
                        // to public-key/content/time matching.
                    }

                    document.getElementById("composerContent").value = "";
                    state.draftMedia = null;
                    renderMediaPreview(null);
                    state.replyingTo = null;
                    updateReplyBanner();
                    state.pendingComment = {
                        submissionId,
                        publicKey,
                        content: signedContent,
                        submittedAt: Date.now(),
                    };
                    status.textContent = t("status_submitted");
                    toast(t("comment_submitted"));
                    setTimeout(() => loadList(), 1200);
                    startPendingSyncPoll();
                } catch (e) {
                    status.textContent = t("status_failed");
                    toast(t("publish_failed") + e.message, "error");
                } finally {
                    btn.disabled = false;
                }
            }

            async function submitEdit(comment, newContent) {
                const cfg = getSettings();
                const chal = await getChallenge(cfg);
                const nonce = await solvePow(chal.prefix, chal.difficulty);
                const { publicKey, signature } = await authorSignature([
                    "PATCH",
                    cfg.siteId,
                    cfg.slug,
                    comment.event_id,
                    newContent,
                    chal.prefix,
                ]);
                const res = await fetch(
                    `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/comments/${encodeURIComponent(comment.event_id)}`,
                    {
                        method: "PATCH",
                        headers: {
                            "Content-Type": "application/json",
                            "Idempotency-Key": newIdempotencyKey(),
                        },
                        body: JSON.stringify({
                            content: newContent,
                            author_public_key: publicKey,
                            author_signature: signature,
                            challenge_response: `${chal.prefix}|${nonce}`,
                        }),
                    },
                );
                if (!res.ok) throw new Error(await apiError(res));
            }

            async function deleteComment(commentId) {
                if (!confirm(t("confirm_delete"))) return;
                const cfg = getSettings();
                try {
                    const chal = await getChallenge(cfg);
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    const { publicKey, signature } = await authorSignature([
                        "DELETE",
                        cfg.siteId,
                        cfg.slug,
                        commentId,
                        chal.prefix,
                    ]);
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/comments?comment_id=${encodeURIComponent(commentId)}`,
                        {
                            method: "DELETE",
                            headers: {
                                "Content-Type": "application/json",
                                "Idempotency-Key": newIdempotencyKey(),
                            },
                            body: JSON.stringify({
                                author_public_key: publicKey,
                                author_signature: signature,
                                challenge_response: `${chal.prefix}|${nonce}`,
                            }),
                        },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    toast(t("comment_deleted"));
                    await loadList();
                } catch (e) {
                    toast(t("delete_failed") + e.message, "error");
                }
            }

            // ==========================================
            // PoW
            // ==========================================

            async function solvePow(prefix, difficulty) {
                let nonce = 0;
                const required = "0".repeat(difficulty);
                const encoder = new TextEncoder();
                while (true) {
                    const input = `${prefix}${nonce}`;
                    const hashBuf = await crypto.subtle.digest(
                        "SHA-256",
                        encoder.encode(input),
                    );
                    const hashHex = Array.from(new Uint8Array(hashBuf))
                        .map((b) => b.toString(16).padStart(2, "0"))
                        .join("");
                    if (hashHex.startsWith(required)) return nonce.toString();
                    nonce += 1;
                    if (nonce % 2000 === 0) {
                        await new Promise((resolve) => setTimeout(resolve, 0));
                    }
                }
            }

            async function sha256Hex(bytes) {
                const hashBuf = await crypto.subtle.digest("SHA-256", bytes);
                return Array.from(new Uint8Array(hashBuf))
                    .map((b) => b.toString(16).padStart(2, "0"))
                    .join("");
            }

            // ==========================================
            // SSE
            // ==========================================

            function connectSse() {
                const cfg = getSettings();
                const url = `${cfg.api}/api/v1/sites/${cfg.siteId}/posts/${cfg.slug}/sse`;
                const sse = new EventSource(url);
                state.currentSse = sse;

                sse.onopen = () => updateSseStatus(true);
                sse.onerror = () => updateSseStatus(false);

                sse.addEventListener("message_created", (event) => {
                    const payload = JSON.parse(event.data).payload;
                    const message = payload.message;
                    toast(t("sse_new_comment") + authorName(message), "success");
                    markPendingSynced([message]);
                    loadList();
                });

                sse.addEventListener("message_updated", (event) => {
                    const payload = JSON.parse(event.data).payload;
                    toast(
                        t("sse_comment_updated") + authorName(payload.message),
                        "success",
                    );
                    loadList();
                });

                sse.addEventListener("message_annotations_changed", () => {
                    // Reactions and poll responses change without the message
                    // content being edited; refresh quietly.
                    loadList();
                });

                sse.addEventListener("message_deleted", () => {
                    toast(t("sse_comment_deleted"), "info");
                    loadList();
                });

                const typingUsers = new Map();
                sse.addEventListener("ephemeral", (event) => {
                    const ev = JSON.parse(event.data);
                    if (ev.type === "typing") {
                        if (ev.typing) {
                            typingUsers.set(ev.user_id, ev.display_name || ev.user_id);
                        } else {
                            typingUsers.delete(ev.user_id);
                        }
                        renderTyping(typingUsers);
                    } else if (ev.type === "presence") {
                        if (ev.presence === "online") {
                            state.presenceOnline.add(ev.user_id);
                        } else {
                            state.presenceOnline.delete(ev.user_id);
                        }
                        updatePresenceIndicator();
                    }
                });
            }

            function updatePresenceIndicator() {
                const el = document.getElementById("roomMemberInfo");
                if (!el) return;
                const online = state.presenceOnline.size;
                const count = el.dataset.memberCount || "0";
                el.textContent = online
                    ? `${count} ${t("members")} · ${online} ${t("members_online")}`
                    : `${count} ${t("members")}`;
            }

            function renderTyping(typingUsers) {
                let el = document.getElementById("typingIndicator");
                if (!el) {
                    el = document.createElement("div");
                    el.id = "typingIndicator";
                    el.className = "text-xs text-slate-400 pl-1 mb-2 min-h-[1rem]";
                    const container = document.getElementById("commentsContainer");
                    container.parentNode.insertBefore(el, container);
                }
                const names = [...typingUsers.values()].slice(0, 3);
                el.textContent = names.length
                    ? `${names.join("、")} ${t("typing_now")}`
                    : "";
            }

            function closeSse() {
                if (state.currentSse) {
                    state.currentSse.close();
                    state.currentSse = null;
                }
                updateSseStatus(false);
            }

            function updateSseStatus(connected) {
                sseConnected = connected;
                const indicator = document.getElementById("sseIndicator");
                const text = document.getElementById("sseText");
                indicator.className = `w-2 h-2 rounded-full ${
                    connected ? "bg-green-500" : "bg-red-400"
                }`;
                text.textContent = connected
                    ? t("sse_connected")
                    : t("sse_disconnected");
            }

            // ==========================================
            // 工具函数
            // ==========================================

            function renderMarkdown(text) {
                if (typeof marked === "undefined") {
                    return `<p>${escapeHtml(text || "")}</p>`;
                }
                const html = marked.parse(text || "");
                if (typeof DOMPurify === "undefined") {
                    // Fail closed: never render unsanitized Markdown HTML.
                    return `<p>${escapeHtml(text || "")}</p>`;
                }
                return DOMPurify.sanitize(html);
            }

            function escapeHtml(value) {
                return String(value)
                    .replace(/&/g, "&amp;")
                    .replace(/</g, "&lt;")
                    .replace(/>/g, "&gt;")
                    .replace(/"/g, "&quot;")
                    .replace(/'/g, "&#39;");
            }

            // The API returns media as same-origin paths (e.g.
            // /api/v1/media/...). Under file:// those resolve against the
            // file origin and fail to load, so absolutize them against the
            // configured API base before using them as img/video/audio src.
            function apiMediaUrl(url) {
                if (!url || typeof url !== "string" || !url.startsWith("/")) {
                    return url;
                }
                const base = (getSettings().api || "").replace(/\/+$/, "");
                return base + url;
            }

            function textBody(content) {
                return content && content.type === "text" ? content.body : "";
            }

            function renderContent(content) {
                if (!content || typeof content !== "object") {
                    return `<span class="text-slate-400">${t("unsupported_message")}</span>`;
                }
                switch (content.type) {
                    case "text":
                        return renderMarkdown(content.body || "");
                    case "media":
                        return renderMedia(content);
                    case "location":
                        return renderLocation(content);
                    case "poll":
                        return renderPoll(content);
                    case "encrypted":
                        return `<span class="italic text-slate-400">${t("encrypted_message")}</span>`;
                    case "unknown":
                        return content.fallback
                            ? renderMarkdown(content.fallback)
                            : `<span class="italic text-slate-400">${t("unsupported_message")}</span>`;
                    default:
                        return `<span class="italic text-slate-400">${t("unsupported_message")}</span>`;
                }
            }

            function renderMedia(media) {
                const url = escapeHtml(apiMediaUrl(media.url || ""));
                const alt = escapeHtml(media.alt_text || media.filename || "");
                switch (media.kind) {
                    case "image":
                        return `<a href="${url}" target="_blank" rel="noopener"><img src="${url}" alt="${alt}" class="max-w-full max-h-96 rounded-lg border border-slate-200" loading="lazy"></a>`;
                    case "sticker":
                        return `<img src="${url}" alt="${alt}" class="max-w-[180px] rounded-lg" loading="lazy">`;
                    case "video":
                        return `<video src="${url}" controls class="max-w-full rounded-lg border border-slate-200"></video>`;
                    case "audio":
                        return `<audio src="${url}" controls class="w-full"></audio>`;
                    case "file":
                        return `<a href="${url}" target="_blank" rel="noopener" class="inline-flex items-center gap-2 border border-slate-200 rounded-lg px-3 py-2 text-sm hover:bg-slate-50">📎 ${escapeHtml(media.filename || media.url)}${media.size ? ` · ${formatBytes(media.size)}` : ""}</a>`;
                    default:
                        return `<a href="${url}" target="_blank" rel="noopener" class="inline-flex items-center gap-2 border border-slate-200 rounded-lg px-3 py-2 text-sm hover:bg-slate-50">📎 ${escapeHtml(media.filename || media.url)}</a>`;
                }
            }

            function renderLocation(location) {
                const geo = location.geo_uri || "";
                const match = geo.match(/^geo:([-\d.]+),([-\d.]+)/);
                const link = match
                    ? `https://www.openstreetmap.org/?mlat=${match[1]}&mlon=${match[2]}#map=16/${match[1]}/${match[2]}`
                    : geo;
                return `<a href="${escapeHtml(link)}" target="_blank" rel="noopener" class="inline-flex items-center gap-2 border border-slate-200 rounded-lg px-3 py-2 text-sm hover:bg-slate-50">📍 ${escapeHtml(location.description || t("open_map"))}</a>`;
            }

            function renderPoll(poll) {
                const responses = poll.responses || [];
                const total = responses.reduce((sum, r) => sum + r.count, 0);
                const rows = (poll.options || [])
                    .map((option, index) => {
                        const summary = responses.find(
                            (r) => r.option_index === index,
                        );
                        const count = summary ? summary.count : 0;
                        const pct = total > 0 ? Math.round((count / total) * 100) : 0;
                        return `<button type="button" class="poll-option w-full text-left space-y-1 hover:bg-indigo-50 rounded-lg p-1.5 transition" data-option="${escapeHtml(option.id)}">
                            <div class="flex justify-between text-xs text-slate-600">
                                <span>${escapeHtml(option.text)}</span>
                                <span>${count} ${t("votes")}</span>
                            </div>
                            <div class="h-2 bg-slate-100 rounded-full">
                                <div class="h-2 bg-indigo-500 rounded-full" style="width:${pct}%"></div>
                            </div>
                        </button>`;
                    })
                    .join("");
                return `<div class="border border-slate-200 rounded-xl p-3 space-y-2 bg-slate-50">
                    <p class="font-medium text-sm">${escapeHtml(poll.question)}</p>
                    ${rows}
                </div>`;
            }

            function renderReactions(reactions) {
                return reactions
                    .map(
                        (reaction) =>
                            `<span class="inline-flex items-center gap-1 text-xs bg-slate-100 rounded-full px-2 py-0.5">${escapeHtml(reaction.key)} ${reaction.count}</span>`,
                    )
                    .join("");
            }

            function formatBytes(bytes) {
                if (!Number.isFinite(bytes)) return "";
                if (bytes < 1024) return `${bytes} B`;
                if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
                return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
            }

            function formatTime(iso) {
                const date = new Date(iso);
                if (Number.isNaN(date.getTime())) return "";
                const diff = Date.now() - date.getTime();
                const minutes = Math.floor(diff / 60000);
                if (minutes < 1) return t("just_now");
                if (minutes < 60) return t("minutes_ago", { n: minutes });
                const hours = Math.floor(minutes / 60);
                if (hours < 24) return t("hours_ago", { n: hours });
                const days = Math.floor(hours / 24);
                if (days < 30) return t("days_ago", { n: days });
                return date.toLocaleDateString(lang === "en" ? "en-US" : "zh-CN");
            }

            function commentAvatarStyle(key) {
                let hash = 0;
                for (const ch of key) {
                    hash = (hash * 31 + ch.codePointAt(0)) % 360;
                }
                return `background:linear-gradient(135deg, hsl(${hash},70%,58%), hsl(${(hash + 45) % 360},70%,46%));`;
            }

            function updateComposerAvatar() {
                const input = document.getElementById("composerDisplayName");
                const avatar = document.getElementById("composerAvatar");
                const name = (input.value || t("guest_default")).trim();
                avatar.textContent = "";
                if (
                    ownAvatarUrl &&
                    ownAvatarSiteId === getSettings().siteId
                ) {
                    const img = document.createElement("img");
                    img.src = ownAvatarUrl;
                    img.alt = "";
                    img.className =
                        "w-9 h-9 rounded-full object-cover shrink-0";
                    img.loading = "lazy";
                    img.addEventListener("error", () => {
                        if (avatar.contains(img)) img.remove();
                        avatar.textContent = name[0].toUpperCase();
                    });
                    avatar.appendChild(img);
                    return;
                }
                avatar.textContent = name[0].toUpperCase();
            }

            function renderSettingsAvatar() {
                const el = document.getElementById("settingsAvatar");
                if (!el) return;
                el.textContent = "";
                const name = (
                    document.getElementById("settingDisplayName").value ||
                    t("guest_default")
                ).trim();
                if (
                    ownAvatarUrl &&
                    ownAvatarSiteId === getSettings().siteId
                ) {
                    const img = document.createElement("img");
                    img.src = ownAvatarUrl;
                    img.alt = "";
                    img.className = "w-12 h-12 rounded-full object-cover";
                    img.loading = "lazy";
                    img.addEventListener("error", () => {
                        renderSettingsAvatarFallback(el, name);
                    });
                    el.appendChild(img);
                    return;
                }
                renderSettingsAvatarFallback(el, name);
            }

            function renderSettingsAvatarFallback(el, name) {
                el.textContent = "";
                const div = document.createElement("div");
                div.className =
                    "w-12 h-12 rounded-full bg-slate-200 flex items-center justify-center text-slate-500 font-bold";
                div.textContent = name[0].toUpperCase();
                el.appendChild(div);
            }

            function avatarCacheKey(siteId) {
                return AVATAR_CACHE_PREFIX + siteId;
            }

            function loadOwnAvatarCache() {
                const cfg = getSettings();
                if (!cfg.siteId) return;
                try {
                    const cached = JSON.parse(
                        localStorage.getItem(avatarCacheKey(cfg.siteId)) ||
                            "null",
                    );
                    if (
                        cached &&
                        typeof cached.url === "string" &&
                        (cached.url.startsWith("/api/v1/media/") ||
                            /^https?:\/\//.test(cached.url))
                    ) {
                        ownAvatarUrl = apiMediaUrl(cached.url);
                        ownAvatarSiteId = cfg.siteId;
                        return;
                    }
                } catch {
                    // corrupted cache; fall through to the comment fallback
                }
                ownAvatarUrl = null;
                ownAvatarSiteId = null;
            }

            function saveOwnAvatarCache(url) {
                const cfg = getSettings();
                if (!cfg.siteId || !url) return;
                try {
                    localStorage.setItem(
                        avatarCacheKey(cfg.siteId),
                        JSON.stringify({
                            url,
                            updatedAt: Date.now(),
                        }),
                    );
                } catch {
                    // cache is best-effort; the in-memory value still works
                }
                ownAvatarUrl = url;
                ownAvatarSiteId = cfg.siteId;
                renderIdentity();
                updateComposerAvatar();
            }

            function clearOwnAvatarCache() {
                const cfg = getSettings();
                ownAvatarUrl = null;
                ownAvatarSiteId = null;
                if (cfg.siteId) {
                    try {
                        localStorage.removeItem(avatarCacheKey(cfg.siteId));
                    } catch {
                        // ignore storage failures
                    }
                }
                renderIdentity();
                updateComposerAvatar();
            }

            // Self-service profile read: the API returns the guest's current
            // display name and avatar for this site, keyed by the Ed25519
            // public key. Used after identity restore/import and when the
            // settings drawer opens, so a restored identity recovers its
            // profile even before posting any comment. Failures are silent:
            // the local avatar cache remains the offline fallback.
            async function refreshOwnProfile() {
                const cfg = getSettings();
                if (!cfg.api || !cfg.siteId || !identity) return;
                try {
                    const params = new URLSearchParams({
                        author_public_key: identity.publicKey,
                    });
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/guests/profile?${params.toString()}`,
                    );
                    if (!res.ok) return;
                    const profile = await res.json();
                    if (!profile) return;
                    if (
                        typeof profile.display_name === "string" &&
                        profile.display_name
                    ) {
                        document.getElementById("composerDisplayName").value =
                            profile.display_name;
                        const settingDisplayName = document.getElementById(
                            "settingDisplayName",
                        );
                        if (settingDisplayName) {
                            settingDisplayName.value = profile.display_name;
                        }
                    }
                    if (profile.avatar_url) {
                        saveOwnAvatarCache(apiMediaUrl(profile.avatar_url));
                    } else {
                        clearOwnAvatarCache();
                    }
                } catch {
                    // offline or unreachable; keep the local cache
                }
            }

            // Own-avatar fallback when the local cache is empty: the newest
            // own comment that carries an avatar (lists are newest-first).
            function refreshOwnAvatarFromComments(comments) {
                const cfg = getSettings();
                if (
                    !cfg.siteId ||
                    !identity ||
                    (ownAvatarUrl && ownAvatarSiteId === cfg.siteId)
                ) {
                    return;
                }
                for (const comment of comments) {
                    if (
                        comment.author &&
                        comment.author.type === "guest" &&
                        comment.author.public_key === identity.publicKey &&
                        comment.author.avatar_url
                    ) {
                        ownAvatarUrl = apiMediaUrl(comment.author.avatar_url);
                        ownAvatarSiteId = cfg.siteId;
                        saveOwnAvatarCache(ownAvatarUrl);
                        return;
                    }
                }
            }

            function chooseAvatarFile() {
                document.getElementById("avatarFile").click();
            }

            async function onAvatarFileSelected(event) {
                const file = event.target.files && event.target.files[0];
                event.target.value = "";
                if (!file) return;
                if (!file.type.startsWith("image/")) {
                    toast(t("avatar_bad_type"), "error");
                    return;
                }
                if (file.size > AVATAR_MAX_BYTES) {
                    toast(t("avatar_too_large"), "error");
                    return;
                }
                try {
                    const { blob, mime, filename } = await readAvatarFile(
                        file,
                    );
                    await uploadAvatar(blob, mime, filename);
                } catch (e) {
                    toast(t("avatar_upload_failed") + e.message, "error");
                }
            }

            async function readAvatarFile(file) {
                // Downscale to a square 512×512 PNG so uploads stay small and
                // the signature covers exactly the bytes that will be sent.
                if (typeof createImageBitmap === "function") {
                    try {
                        const bitmap = await createImageBitmap(file);
                        try {
                            const side = Math.min(
                                AVATAR_MAX_DIMENSION,
                                Math.max(bitmap.width, bitmap.height),
                            );
                            const canvas = document.createElement("canvas");
                            canvas.width = side;
                            canvas.height = side;
                            const ctx = canvas.getContext("2d");
                            const scale = Math.max(
                                side / bitmap.width,
                                side / bitmap.height,
                            );
                            const width = bitmap.width * scale;
                            const height = bitmap.height * scale;
                            ctx.drawImage(
                                bitmap,
                                (side - width) / 2,
                                (side - height) / 2,
                                width,
                                height,
                            );
                            const blob = await new Promise((resolve) =>
                                canvas.toBlob(resolve, "image/png"),
                            );
                            if (blob) {
                                return {
                                    blob,
                                    mime: "image/png",
                                    filename: "avatar.png",
                                };
                            }
                        } finally {
                            bitmap.close();
                        }
                    } catch {
                        // fall through to the original file
                    }
                }
                return {
                    blob: file,
                    mime: file.type || "image/png",
                    filename: file.name || "avatar",
                };
            }

            async function uploadAvatar(blob, mime, filename) {
                const cfg = getSettings();
                const chal = await getChallenge(cfg);
                const nonce = await solvePow(chal.prefix, chal.difficulty);
                const contentHash = await sha256Hex(
                    await blob.arrayBuffer(),
                );
                const { publicKey, signature } = await authorSignature([
                    "UPLOAD_AVATAR",
                    cfg.siteId,
                    mime,
                    contentHash,
                    chal.prefix,
                ]);
                const params = new URLSearchParams({
                    author_public_key: publicKey,
                    author_signature: signature,
                    challenge_response: `${chal.prefix}|${nonce}`,
                    mime,
                    filename,
                });
                const res = await fetch(
                    `${cfg.api}/api/v1/sites/${cfg.siteId}/me/avatar?${params.toString()}`,
                    {
                        method: "PUT",
                        headers: {
                            "Content-Type": mime,
                            "Idempotency-Key": newIdempotencyKey(),
                        },
                        body: blob,
                    },
                );
                if (!res.ok) throw new Error(await apiError(res));
                const data = await res.json();
                if (!data.avatar_url) {
                    throw new Error(t("avatar_upload_failed"));
                }
                saveOwnAvatarCache(apiMediaUrl(data.avatar_url));
                toast(t("avatar_uploaded"), "success");
            }

            async function removeAvatar() {
                const cfg = getSettings();
                try {
                    const chal = await getChallenge(cfg);
                    const nonce = await solvePow(chal.prefix, chal.difficulty);
                    const { publicKey, signature } = await authorSignature([
                        "DELETE_AVATAR",
                        cfg.siteId,
                        chal.prefix,
                    ]);
                    const params = new URLSearchParams({
                        author_public_key: publicKey,
                        author_signature: signature,
                        challenge_response: `${chal.prefix}|${nonce}`,
                    });
                    const res = await fetch(
                        `${cfg.api}/api/v1/sites/${cfg.siteId}/me/avatar?${params.toString()}`,
                        { method: "DELETE" },
                    );
                    if (!res.ok) throw new Error(await apiError(res));
                    clearOwnAvatarCache();
                    toast(t("avatar_removed"), "success");
                } catch (e) {
                    toast(t("avatar_remove_failed") + e.message, "error");
                }
            }

            function toast(message, type = "info") {
                const el = document.createElement("div");
                const bg =
                    type === "error"
                        ? "bg-red-600"
                        : type === "success"
                          ? "bg-emerald-600"
                          : "bg-slate-800";
                el.className = `${bg} text-white text-sm rounded-lg shadow-lg px-4 py-2.5 opacity-0 translate-y-1`;
                el.textContent = message;
                document.getElementById("toasts").appendChild(el);
                requestAnimationFrame(() => {
                    el.classList.remove("opacity-0", "translate-y-1");
                });
                setTimeout(() => {
                    el.classList.add("opacity-0");
                    setTimeout(() => el.remove(), 300);
                }, 3200);
            }

            // ==========================================
            // 入口
            // ==========================================

            async function initApp() {
                const cfg = getSettings();
                sseKey = `${cfg.api}|${cfg.siteId}|${cfg.slug}`;
                closeSse();
                loadOwnAvatarCache();
                state.currentPage = 1;
                state.meta = null;
                showLoading();
                await loadRoles();
                await loadList();
                connectSse();
                await loadStickers();
            }

            // ==========================================
            // 静态事件绑定
            // ==========================================

            function bindStaticEvents() {
                document
                    .getElementById("langBtn")
                    .addEventListener("click", toggleLang);
                document
                    .getElementById("headerSettingsBtn")
                    .addEventListener("click", openSettings);
                document
                    .getElementById("tabAll")
                    .addEventListener("click", () => switchTab("all"));
                document
                    .getElementById("tabMine")
                    .addEventListener("click", () => switchTab("mine"));
                document
                    .getElementById("composerDisplayName")
                    .addEventListener("input", updateComposerAvatar);
                document
                    .getElementById("cancelReplyBtn")
                    .addEventListener("click", cancelReply);
                document
                    .getElementById("submitBtn")
                    .addEventListener("click", submitComment);
                document
                    .getElementById("prevBtn")
                    .addEventListener("click", () => changePage(-1));
                document
                    .getElementById("nextBtn")
                    .addEventListener("click", () => changePage(1));
                document
                    .getElementById("settingsModal")
                    .addEventListener("click", function (event) {
                        if (event.target === this) closeSettings();
                    });
                document
                    .getElementById("settingsCloseBtn")
                    .addEventListener("click", closeSettings);
                document
                    .getElementById("copyPublicKeyBtn")
                    .addEventListener("click", copyPublicKey);
                document
                    .getElementById("avatarFile")
                    .addEventListener("change", onAvatarFileSelected);
                document
                    .getElementById("uploadAvatarBtn")
                    .addEventListener("click", chooseAvatarFile);
                document
                    .getElementById("removeAvatarBtn")
                    .addEventListener("click", removeAvatar);
                document
                    .getElementById("showMnemonicBtn")
                    .addEventListener("click", showMnemonic);
                document
                    .getElementById("exportPrivateKeyBtn")
                    .addEventListener("click", exportPrivateKey);
                document
                    .getElementById("toggleMnemonicRestoreBtn")
                    .addEventListener("click", toggleMnemonicRestore);
                document
                    .getElementById("importPrivateKeyBtn")
                    .addEventListener("click", () =>
                        document.getElementById("privateKeyFile").click(),
                    );
                document
                    .getElementById("restoreFromMnemonicBtn")
                    .addEventListener("click", restoreFromMnemonic);
                document
                    .getElementById("privateKeyFile")
                    .addEventListener("change", importPrivateKeyFile);
                document
                    .getElementById("resetIdentityBtn")
                    .addEventListener("click", resetIdentity);
                document
                    .getElementById("settingsCancelBtn")
                    .addEventListener("click", closeSettings);
                document
                    .getElementById("saveSettingsBtn")
                    .addEventListener("click", saveSettings);
                document
                    .getElementById("copyMnemonicBtn")
                    .addEventListener("click", copyMnemonic);
                document
                    .getElementById("mnemonicModalPrimary")
                    .addEventListener("click", acknowledgeMnemonicBackup);
                document
                    .getElementById("mnemonicModalAlt")
                    .addEventListener("click", discardMnemonicBackup);
                document
                    .getElementById("imageBtn")
                    .addEventListener("click", pickImage);
                document
                    .getElementById("voiceBtn")
                    .addEventListener("click", toggleVoiceRecord);
                document
                    .getElementById("stickerBtn")
                    .addEventListener("click", pickSticker);
                document
                    .getElementById("fileBtn")
                    .addEventListener("click", () =>
                        document.getElementById("fileInput").click(),
                    );
                document
                    .getElementById("locationBtn")
                    .addEventListener("click", sendLocation);
                document
                    .getElementById("imageInput")
                    .addEventListener("change", (event) => {
                        const file = event.target.files && event.target.files[0];
                        if (file) {
                            attachMedia(
                                file,
                                file.name,
                                file.type || "application/octet-stream",
                                false,
                            );
                        }
                        event.target.value = "";
                    });
                document
                    .getElementById("fileInput")
                    .addEventListener("change", (event) => {
                        const file = event.target.files && event.target.files[0];
                        if (file) {
                            attachMedia(
                                file,
                                file.name,
                                file.type || "application/octet-stream",
                                false,
                            );
                        }
                        event.target.value = "";
                    });
            }

            bindStaticEvents();
