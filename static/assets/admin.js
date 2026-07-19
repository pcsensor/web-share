(() => {
  "use strict";

  const $ = (s, r = document) => r.querySelector(s);
  const usersBody = $("#users-body");
  const usersEmpty = $("#users-empty");
  const auditBody = $("#audit-body");
  const auditEmpty = $("#audit-empty");
  const usersPanel = $("#users-panel");
  const auditPanel = $("#audit-panel");
  const invitesPanel = $("#invites-panel");
  const invitesBody = $("#invites-body");
  const invitesEmpty = $("#invites-empty");
  const inviteFresh = $("#invite-fresh");
  const inviteFreshCode = $("#invite-fresh-code");
  const errorEl = $("#admin-error");
  const toastRegion = $("#toast-region");

  let filter = "pending_approval";
  let me = null;

  async function api(path, options = {}) {
    const res = await fetch(path, {
      credentials: "same-origin",
      headers: options.body
        ? { "Content-Type": "application/json", ...(options.headers || {}) }
        : options.headers,
      ...options,
    });
    if (res.status === 204) return null;
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      const err = new Error(data.error || res.statusText || "请求失败");
      err.status = res.status;
      throw err;
    }
    return data;
  }

  function showError(msg) {
    if (!msg) {
      errorEl.classList.add("hidden");
      return;
    }
    errorEl.textContent = msg;
    errorEl.classList.remove("hidden");
  }

  function toast(message, type = "error") {
    const el = document.createElement("div");
    el.className = `toast ${type}`;
    el.textContent = message;
    toastRegion.appendChild(el);
    setTimeout(() => {
      el.classList.add("is-leaving");
      setTimeout(() => el.remove(), 350);
    }, 3200);
  }

  const STATUS_LABEL = {
    pending_approval: ["待审核", "pending"],
    rejected: ["已拒绝", "rejected"],
    approved_unbound: ["待绑定验证器", "unbound"],
    active: ["已激活", "active"],
    disabled: ["已停用", "disabled"],
  };

  function statusBadge(status) {
    const [label, cls] = STATUS_LABEL[status] || [status, ""];
    return `<span class="badge ${cls}">${label}</span>`;
  }

  function inviteBadge(status) {
    const map = {
      unused: ["未使用", "active"],
      used: ["已使用", "pending"],
      revoked: ["已吊销", "disabled"],
      expired: ["已过期", "rejected"],
    };
    const [label, cls] = map[status] || [status, ""];
    return `<span class="badge ${cls}">${label}</span>`;
  }

  function formatTime(ts) {
    try {
      return new Date(ts).toLocaleString();
    } catch {
      return ts || "—";
    }
  }

  function opsFor(user) {
    const buttons = [];
    if (user.status === "pending_approval" || user.status === "rejected") {
      buttons.push(`<button type="button" data-act="approve" data-id="${user.id}">通过</button>`);
      if (user.status === "pending_approval") {
        buttons.push(`<button type="button" class="danger" data-act="reject" data-id="${user.id}">拒绝</button>`);
      }
    }
    if (user.status === "active" || user.status === "approved_unbound") {
      if (user.role !== "admin") {
        buttons.push(`<button type="button" class="danger" data-act="disable" data-id="${user.id}">停用</button>`);
      }
      if (user.totp_enabled || user.status === "active") {
        buttons.push(`<button type="button" data-act="reset-totp" data-id="${user.id}">重置验证器</button>`);
      }
    }
    if (user.status === "disabled") {
      buttons.push(`<button type="button" data-act="enable" data-id="${user.id}">启用</button>`);
    }
    if (!buttons.length) return "—";
    return `<div class="ops">${buttons.join("")}</div>`;
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  async function loadUsers() {
    showError("");
    const qs = filter ? `?status=${encodeURIComponent(filter)}` : "";
    const users = await api(`/api/admin/users${qs}`);
    usersBody.replaceChildren();
    if (!users.length) {
      usersEmpty.classList.remove("hidden");
      return;
    }
    usersEmpty.classList.add("hidden");
    for (const u of users) {
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td>
          <strong>${escapeHtml(u.username)}</strong>
          ${u.role === "admin" ? ' <span class="badge">管理员</span>' : ""}
        </td>
        <td>${escapeHtml(u.display_name)}</td>
        <td>${statusBadge(u.status)}</td>
        <td>${formatTime(u.created_at)}</td>
        <td>${opsFor(u)}</td>`;
      usersBody.appendChild(tr);
    }
  }

  async function loadAudit() {
    showError("");
    const rows = await api("/api/admin/audit");
    auditBody.replaceChildren();
    if (!rows.length) {
      auditEmpty.classList.remove("hidden");
      return;
    }
    auditEmpty.classList.add("hidden");
    for (const r of rows) {
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td>${formatTime(r.created_at)}</td>
        <td><code>${escapeHtml(r.action)}</code></td>
        <td><code>${escapeHtml(String(r.admin_id).slice(0, 8))}…</code></td>
        <td>${r.target_id ? `<code>${escapeHtml(String(r.target_id).slice(0, 8))}…</code>` : "—"}</td>`;
      auditBody.appendChild(tr);
    }
  }

  async function loadInvites() {
    showError("");
    const rows = await api("/api/admin/invites");
    invitesBody.replaceChildren();
    if (!rows.length) {
      invitesEmpty.classList.remove("hidden");
      return;
    }
    invitesEmpty.classList.add("hidden");
    for (const inv of rows) {
      const tr = document.createElement("tr");
      const canRevoke = inv.status === "unused";
      tr.innerHTML = `
        <td><code class="invite-code-cell">${escapeHtml(inv.code)}</code>
          ${inv.status === "unused" ? `<button type="button" class="linkish" data-copy="${escapeHtml(inv.code)}">复制</button>` : ""}
        </td>
        <td>${inviteBadge(inv.status)}</td>
        <td>${formatTime(inv.created_at)}</td>
        <td>${formatTime(inv.expires_at)}</td>
        <td>${inv.used_at ? formatTime(inv.used_at) : "—"}</td>
        <td>${canRevoke
          ? `<div class="ops"><button type="button" class="danger" data-revoke="${inv.id}">取消</button></div>`
          : "—"}</td>`;
      invitesBody.appendChild(tr);
    }
  }

  function showPanels() {
    usersPanel.classList.add("hidden");
    auditPanel.classList.add("hidden");
    invitesPanel.classList.add("hidden");
    if (filter === "__audit") {
      auditPanel.classList.remove("hidden");
    } else if (filter === "__invites") {
      invitesPanel.classList.remove("hidden");
    } else {
      usersPanel.classList.remove("hidden");
    }
  }

  async function refresh() {
    try {
      showPanels();
      if (filter === "__audit") {
        await loadAudit();
      } else if (filter === "__invites") {
        await loadInvites();
      } else {
        await loadUsers();
      }
    } catch (err) {
      if (err.status === 401 || err.status === 403) {
        showError(err.message || "无权限访问管理后台，请使用管理员账号登录");
        return;
      }
      showError(err.message || "加载失败");
    }
  }

  document.querySelectorAll(".admin-tab").forEach((tab) => {
    tab.addEventListener("click", () => {
      document.querySelectorAll(".admin-tab").forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");
      filter = tab.dataset.filter ?? "";
      refresh();
    });
  });

  usersBody.addEventListener("click", async (event) => {
    const btn = event.target.closest("button[data-act]");
    if (!btn) return;
    const act = btn.dataset.act;
    const id = btn.dataset.id;
    const map = {
      approve: "approve",
      reject: "reject",
      disable: "disable",
      enable: "enable",
      "reset-totp": "reset-totp",
    };
    const path = map[act];
    if (!path) return;
    if (act === "reset-totp" && !confirm("确认重置该用户的验证器？对方需重新绑定。")) return;
    if (act === "disable" && !confirm("确认停用该用户？")) return;
    btn.disabled = true;
    try {
      await api(`/api/admin/users/${id}/${path}`, { method: "POST" });
      toast("操作成功", "success");
      await refresh();
    } catch (err) {
      toast(err.message || "操作失败");
      btn.disabled = false;
    }
  });

  invitesBody.addEventListener("click", async (event) => {
    const copyBtn = event.target.closest("button[data-copy]");
    if (copyBtn) {
      try {
        await navigator.clipboard.writeText(copyBtn.dataset.copy);
        toast("已复制邀请码", "success");
      } catch {
        toast("复制失败，请手动选择");
      }
      return;
    }
    const revokeBtn = event.target.closest("button[data-revoke]");
    if (!revokeBtn) return;
    if (!confirm("确定吊销该邀请码？")) return;
    revokeBtn.disabled = true;
    try {
      await api(`/api/admin/invites/${revokeBtn.dataset.revoke}`, { method: "DELETE" });
      toast("已吊销", "success");
      await loadInvites();
    } catch (err) {
      toast(err.message || "吊销失败");
      revokeBtn.disabled = false;
    }
  });

  $("#create-invite-btn")?.addEventListener("click", async () => {
    const btn = $("#create-invite-btn");
    btn.disabled = true;
    try {
      const data = await api("/api/admin/invites", { method: "POST", body: "{}" });
      const code = data.invite?.code || "";
      inviteFreshCode.textContent = code;
      const expEl = $("#invite-fresh-exp");
      if (expEl && data.invite?.expires_at) {
        expEl.textContent = `有效至 ${formatTime(data.invite.expires_at)}`;
      }
      inviteFresh.classList.remove("hidden");
      toast("邀请码已生成（24 小时内有效）", "success");
      try {
        await navigator.clipboard.writeText(code);
        toast("已复制到剪贴板", "success");
      } catch { /* ignore */ }
      await loadInvites();
    } catch (err) {
      toast(err.message || "生成失败");
    } finally {
      btn.disabled = false;
    }
  });

  $("#copy-invite-btn")?.addEventListener("click", async () => {
    const code = inviteFreshCode.textContent;
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code);
      toast("已复制", "success");
    } catch {
      toast("复制失败");
    }
  });

  const adminAuth = $("#admin-auth");
  const adminMain = $("#admin-main");
  const authLogin = $("#admin-auth-login");
  const auth2fa = $("#admin-auth-2fa");
  const authTotp = $("#admin-auth-totp");

  function setBtnLoading(btn, on) {
    if (!btn) return;
    btn.disabled = on;
    btn.classList.toggle("is-loading", on);
  }

  function showErr(el, msg) {
    if (!el) return;
    if (!msg) {
      el.classList.add("hidden");
      return;
    }
    el.textContent = msg;
    el.classList.remove("hidden");
  }

  function showAuthPanel(name) {
    adminAuth?.classList.remove("hidden");
    adminMain?.classList.add("hidden");
    [authLogin, auth2fa, authTotp].forEach((p) => p?.classList.add("hidden"));
    if (name === "login") authLogin?.classList.remove("hidden");
    if (name === "2fa") auth2fa?.classList.remove("hidden");
    if (name === "totp") authTotp?.classList.remove("hidden");
  }

  function showMain() {
    adminAuth?.classList.add("hidden");
    adminMain?.classList.remove("hidden");
    showError("");
  }

  async function enterIfReady(user) {
    me = user;
    if (user.role !== "admin") {
      showAuthPanel("login");
      showError("当前账号不是管理员。请使用管理员用户名登录。");
      return false;
    }
    if (user.next_step === "chat") {
      showMain();
      await refresh();
      return true;
    }
    if (user.next_step === "verify_2fa") {
      showAuthPanel("2fa");
      showError("请完成身份验证后进入后台。");
      return false;
    }
    if (user.next_step === "setup_totp") {
      showAuthPanel("totp");
      showError("请先绑定身份验证器（管理员同样需要）。");
      await startAdminTotp();
      return false;
    }
    showAuthPanel("login");
    showError(`当前状态无法进入后台（${user.next_step || user.status || "unknown"}）。`);
    return false;
  }

  async function startAdminTotp() {
    const qr = $("#admin-totp-qr");
    const secret = $("#admin-totp-secret");
    if (qr) qr.innerHTML = "";
    if (secret) secret.textContent = "加载中…";
    try {
      const data = await api("/api/auth/totp/setup/start", { method: "POST" });
      if (secret) secret.textContent = data.secret_base32 || "";
      if (qr && data.qr_svg) qr.innerHTML = data.qr_svg;
    } catch (err) {
      showErr($("#admin-totp-error"), err.message || "无法开始绑定");
    }
  }

  $("#admin-login-form")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    showErr($("#admin-login-error"));
    showError("");
    const username = $("#admin-user").value.trim();
    const password = $("#admin-pass").value;
    if (!username || !password) {
      showErr($("#admin-login-error"), "请输入用户名和密码");
      return;
    }
    const btn = $("#admin-login-btn");
    setBtnLoading(btn, true);
    try {
      const data = await api("/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ username, password }),
      });
      $("#admin-pass").value = "";
      await enterIfReady(data.user);
    } catch (err) {
      const msg = err.status === 401
        ? "用户名或密码错误（注意：.env 里的 bootstrap 密码仅首次创建时生效）"
        : err.message || "登录失败";
      showErr($("#admin-login-error"), msg);
    } finally {
      setBtnLoading(btn, false);
    }
  });

  $("#admin-2fa-form")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    showErr($("#admin-2fa-error"));
    const code = $("#admin-2fa-code").value.trim();
    if (code.length !== 6) {
      showErr($("#admin-2fa-error"), "请输入 6 位验证码");
      return;
    }
    const btn = $("#admin-2fa-btn");
    setBtnLoading(btn, true);
    try {
      const data = await api("/api/auth/2fa/verify", {
        method: "POST",
        body: JSON.stringify({
          code,
          trust_device: Boolean($("#admin-trust")?.checked),
        }),
      });
      $("#admin-2fa-code").value = "";
      await enterIfReady(data.user);
    } catch (err) {
      showErr($("#admin-2fa-error"), err.message || "验证失败");
    } finally {
      setBtnLoading(btn, false);
    }
  });

  $("#admin-totp-form")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    showErr($("#admin-totp-error"));
    const code = $("#admin-totp-code").value.trim();
    if (code.length !== 6) {
      showErr($("#admin-totp-error"), "请输入 6 位验证码");
      return;
    }
    const btn = $("#admin-totp-btn");
    setBtnLoading(btn, true);
    try {
      const data = await api("/api/auth/totp/setup/confirm", {
        method: "POST",
        body: JSON.stringify({ code }),
      });
      if (data.recovery_codes?.length) {
        toast(`请妥善保存恢复码：${data.recovery_codes.slice(0, 3).join(", ")}…`, "success");
      }
      await enterIfReady(data.user);
    } catch (err) {
      showErr($("#admin-totp-error"), err.message || "绑定失败");
    } finally {
      setBtnLoading(btn, false);
    }
  });

  $("#refresh-btn").addEventListener("click", () => {
    if (me?.next_step === "chat") refresh();
  });
  $("#logout-btn").addEventListener("click", async () => {
    try {
      await api("/api/logout", { method: "POST" });
    } catch { /* ignore */ }
    me = null;
    showAuthPanel("login");
    showError("已退出，请重新登录管理员账号。");
  });

  async function loadPublicConfig() {
    try {
      const cfg = await api("/api/config");
      const days = Number(cfg.device_trust_days) || 60;
      const label = $("#admin-trust-label");
      if (label) label.textContent = `信任此设备 ${days} 天`;
      const hint = document.querySelector("#admin-auth-2fa .invite-hint");
      if (hint && !hint.dataset.base) {
        hint.dataset.base = hint.textContent;
      }
    } catch { /* keep default label */ }
  }

  (async () => {
    await loadPublicConfig();
    try {
      const user = await api("/api/me");
      await enterIfReady(user);
    } catch {
      showAuthPanel("login");
      // No auto-redirect to / — stay on /admin so admin can log in here.
    }
  })();
})();
