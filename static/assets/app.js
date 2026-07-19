(() => {
  "use strict";

  const $ = (selector, root = document) => root.querySelector(selector);
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  const loginView = $("#login-view");
  const chatView = $("#chat-view");

  const panels = {
    login: $("#panel-login"),
    register: $("#panel-register"),
    status: $("#panel-status"),
    totpSetup: $("#panel-totp-setup"),
    recovery: $("#panel-recovery"),
    twofa: $("#panel-2fa"),
  };

  const loginForm = $("#login-form");
  const loginError = $("#login-error");
  const loginBtn = $("#login-btn");
  const loginUsername = $("#login-username");
  const loginPassword = $("#login-password");
  const loginPasswordToggle = $("#login-password-toggle");

  const registerForm = $("#register-form");
  const registerError = $("#register-error");
  const registerBtn = $("#register-btn");

  const statusTitle = $("#status-title");
  const statusMessage = $("#status-message");
  const statusLogoutBtn = $("#status-logout-btn");

  const totpQr = $("#totp-qr");
  const totpSecret = $("#totp-secret");
  const totpSetupForm = $("#totp-setup-form");
  const totpSetupCode = $("#totp-setup-code");
  const totpSetupError = $("#totp-setup-error");
  const totpSetupBtn = $("#totp-setup-btn");

  const recoveryList = $("#recovery-codes-list");
  const recoveryContinueBtn = $("#recovery-continue-btn");

  const twofaForm = $("#twofa-form");
  const twofaCode = $("#twofa-code");
  const twofaError = $("#twofa-error");
  const twofaBtn = $("#twofa-btn");
  const trustDevice = $("#trust-device");
  const recoverForm = $("#recover-form");
  const recoverCode = $("#recover-code");
  const recoverError = $("#recover-error");

  const chatSurface = $("#chat-surface");
  const messagesEl = $("#messages");
  const messageInput = $("#message-input");
  const sendBtn = $("#send-btn");
  const attachBtn = $("#attach-btn");
  const fileInput = $("#file-input");
  const logoutButtons = [$("#logout-btn"), $("#mobile-logout-btn")].filter(Boolean);
  const onlineCount = $("#online-count");
  const sidebarOnlineCount = $("#sidebar-online-count");
  const myName = $("#my-name");
  const sideAvatar = $("#side-avatar");
  const adminLink = $("#admin-link");
  const connectionDot = $("#connection-dot");
  const connectionLabel = $("#connection-label");
  const sidebarPresence = $(".room-copy .presence-dot");

  const previewBar = $("#preview-bar");
  const previewLabel = $("#preview-label");
  const previewCancel = $("#preview-cancel");
  const uploadProgress = $("#upload-progress");
  const uploadBar = $("#upload-bar");
  const uploadText = $("#upload-text");
  const scrollLatest = $("#scroll-latest");

  const lightbox = $("#lightbox");
  const lightboxBody = $("#lightbox-body");
  const lightboxClose = $("#lightbox-close");
  const toastRegion = $("#toast-region");

  let me = null;
  let pendingRecoveryCodes = null;
  let ws = null;
  let wsRetry = 0;
  let wsGeneration = 0;
  let reconnectTimer = null;
  let pendingFile = null;
  let uploading = false;
  let activeUpload = null;
  let dragDepth = 0;
  let lightboxTrigger = null;
  const seenIds = new Set();

  // ---------- API ----------
  async function api(path, options = {}) {
    const res = await fetch(path, {
      credentials: "same-origin",
      headers: options.body && !(options.body instanceof FormData)
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

  // ---------- Views and auth ----------
  function swapView(target, animate = true) {
    const apply = () => {
      const showChatView = target === "chat";
      loginView.classList.toggle("hidden", showChatView);
      chatView.classList.toggle("hidden", !showChatView);
    };

    if (animate && document.startViewTransition && !reducedMotion.matches) {
      try {
        document.startViewTransition(apply);
        return;
      } catch {
        // Fall through when another view transition is already active.
      }
    }
    apply();
  }

  function showAuthPanel(name) {
    Object.entries(panels).forEach(([key, el]) => {
      if (el) el.classList.toggle("hidden", key !== name);
    });
    swapView("login", false);
  }

  function routeByUser(user, animate = true) {
    me = user;
    const step = user.next_step;

    if (step === "chat") {
      if (pendingRecoveryCodes?.length) {
        showRecoveryCodes(pendingRecoveryCodes);
        return;
      }
      showChat(animate);
      return;
    }

    closeWs();
    setConnection("未连接", "offline");
    updateOnlineCount(0);

    if (step === "wait_approval") {
      statusTitle.textContent = "等待审核";
      statusMessage.textContent = "你的账号已提交，管理员通过后即可登录绑定验证器。";
      showAuthPanel("status");
      return;
    }
    if (step === "rejected") {
      statusTitle.textContent = "未通过审核";
      statusMessage.textContent = "账号未通过审核，请联系管理员。";
      showAuthPanel("status");
      return;
    }
    if (step === "disabled") {
      statusTitle.textContent = "账号已停用";
      statusMessage.textContent = "你的账号已被管理员停用。";
      showAuthPanel("status");
      return;
    }
    if (step === "setup_totp") {
      showAuthPanel("totpSetup");
      startTotpSetup();
      return;
    }
    if (step === "verify_2fa") {
      showAuthPanel("twofa");
      applyPublicConfig(publicConfig);
      window.requestAnimationFrame(() => twofaCode?.focus({ preventScroll: true }));
      return;
    }

    showAuthPanel("login");
  }

  async function trySession() {
    try {
      const user = await api("/api/me");
      routeByUser(user, false);
      return true;
    } catch {
      me = null;
      showAuthPanel("login");
      return false;
    }
  }

  function showChat(animate = true) {
    const nickname = me?.nickname || "访客";
    myName.textContent = nickname;
    sideAvatar.textContent = getInitials(nickname);
    sideAvatar.style.setProperty("--avatar-hue", String(hueFor(nickname)));
    if (adminLink) {
      adminLink.classList.toggle("hidden", me?.role !== "admin");
    }
    swapView("chat", animate);
    updateComposerState();
    connectWs();
    window.requestAnimationFrame(() => messageInput.focus({ preventScroll: true }));
  }

  function setButtonLoading(btn, value) {
    if (!btn) return;
    btn.disabled = value;
    btn.classList.toggle("is-loading", value);
    btn.setAttribute("aria-busy", String(value));
  }

  function showFormError(el, form, message) {
    if (!el) return;
    el.textContent = message;
    el.classList.remove("hidden");
    if (form) {
      form.classList.remove("login-form-error");
      void form.offsetWidth;
      form.classList.add("login-form-error");
    }
  }

  function clearFormError(el, form) {
    if (el) el.classList.add("hidden");
    form?.classList.remove("login-form-error");
  }

  $("#goto-register")?.addEventListener("click", () => {
    clearFormError(loginError, loginForm);
    showAuthPanel("register");
  });
  $("#goto-login")?.addEventListener("click", () => {
    clearFormError(registerError, registerForm);
    showAuthPanel("login");
  });

  loginForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(loginError, loginForm);

    const username = loginUsername.value.trim();
    const password = loginPassword.value;
    if (!username || !password) {
      showFormError(loginError, loginForm, !username ? "请输入用户名" : "请输入密码");
      return;
    }

    setButtonLoading(loginBtn, true);
    try {
      const data = await api("/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ username, password }),
      });
      loginPassword.value = "";
      routeByUser(data.user, true);
    } catch (err) {
      const message = err.status === 429
        ? "尝试次数过多，请稍后再试"
        : err.status === 401
          ? "用户名或密码错误（管理员：.env 中 bootstrap 密码仅首次建库生效）"
          : err.message || "登录失败，请稍后重试";
      showFormError(loginError, loginForm, message);
      loginPassword.select();
    } finally {
      setButtonLoading(loginBtn, false);
    }
  });

  registerForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(registerError, registerForm);

    const username = $("#reg-username").value.trim();
    const display_name = $("#reg-display-name").value.trim();
    const password = $("#reg-password").value;
    const password2 = $("#reg-password2").value;
    const invite_code = ($("#reg-invite")?.value || "").trim();

    if (!username || !display_name || !password) {
      showFormError(registerError, registerForm, "请填写完整信息");
      return;
    }
    if (password !== password2) {
      showFormError(registerError, registerForm, "两次密码不一致");
      return;
    }

    setButtonLoading(registerBtn, true);
    try {
      const data = await api("/api/auth/register", {
        method: "POST",
        body: JSON.stringify({
          username,
          password,
          display_name,
          invite_code: invite_code || null,
        }),
      });
      const msg = data?.message
        || (data?.via_invite
          ? "注册成功，请登录并绑定身份验证器"
          : "注册成功，请等待管理员审核后登录");
      showToast(msg, "success");
      registerForm.reset();
      showAuthPanel("login");
    } catch (err) {
      showFormError(registerError, registerForm, err.message || "注册失败");
    } finally {
      setButtonLoading(registerBtn, false);
    }
  });

  loginPasswordToggle?.addEventListener("click", () => {
    const willShow = loginPassword.type === "password";
    loginPassword.type = willShow ? "text" : "password";
    loginPasswordToggle.classList.toggle("is-visible", willShow);
    loginPasswordToggle.setAttribute("aria-pressed", String(willShow));
    loginPasswordToggle.setAttribute("aria-label", willShow ? "隐藏密码" : "显示密码");
    loginPassword.focus({ preventScroll: true });
  });

  async function startTotpSetup() {
    totpQr.innerHTML = "";
    totpSecret.textContent = "加载中…";
    try {
      const data = await api("/api/auth/totp/setup/start", { method: "POST" });
      totpSecret.textContent = data.secret_base32 || "";
      if (data.qr_svg) {
        totpQr.innerHTML = data.qr_svg;
      }
      totpSetupCode.value = "";
      window.requestAnimationFrame(() => totpSetupCode.focus({ preventScroll: true }));
    } catch (err) {
      showFormError(totpSetupError, totpSetupForm, err.message || "无法开始绑定");
    }
  }

  totpSetupForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(totpSetupError, totpSetupForm);
    const code = totpSetupCode.value.trim();
    if (code.length !== 6) {
      showFormError(totpSetupError, totpSetupForm, "请输入 6 位验证码");
      return;
    }
    setButtonLoading(totpSetupBtn, true);
    try {
      const data = await api("/api/auth/totp/setup/confirm", {
        method: "POST",
        body: JSON.stringify({ code }),
      });
      if (data.recovery_codes?.length) {
        pendingRecoveryCodes = data.recovery_codes;
      }
      routeByUser(data.user, true);
    } catch (err) {
      showFormError(totpSetupError, totpSetupForm, err.message || "绑定失败");
    } finally {
      setButtonLoading(totpSetupBtn, false);
    }
  });

  function showRecoveryCodes(codes) {
    recoveryList.replaceChildren();
    codes.forEach((code) => {
      const li = document.createElement("li");
      li.textContent = code;
      recoveryList.appendChild(li);
    });
    showAuthPanel("recovery");
  }

  recoveryContinueBtn?.addEventListener("click", () => {
    pendingRecoveryCodes = null;
    if (me) showChat(true);
  });

  twofaForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(twofaError, twofaForm);
    const code = twofaCode.value.trim();
    if (code.length !== 6) {
      showFormError(twofaError, twofaForm, "请输入 6 位验证码");
      return;
    }
    setButtonLoading(twofaBtn, true);
    try {
      const data = await api("/api/auth/2fa/verify", {
        method: "POST",
        body: JSON.stringify({
          code,
          trust_device: Boolean(trustDevice?.checked),
        }),
      });
      twofaCode.value = "";
      routeByUser(data.user, true);
    } catch (err) {
      showFormError(twofaError, twofaForm, err.message || "验证失败");
    } finally {
      setButtonLoading(twofaBtn, false);
    }
  });

  recoverForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(recoverError, recoverForm);
    const recovery_code = recoverCode.value.trim();
    if (!recovery_code) {
      showFormError(recoverError, recoverForm, "请输入恢复码");
      return;
    }
    try {
      const data = await api("/api/auth/2fa/recover", {
        method: "POST",
        body: JSON.stringify({ recovery_code }),
      });
      recoverCode.value = "";
      showToast("恢复码已使用，请重新绑定验证器", "success");
      routeByUser(data.user, true);
    } catch (err) {
      showFormError(recoverError, recoverForm, err.message || "恢复失败");
    }
  });

  async function logout() {
    if (activeUpload) {
      activeUpload.abort();
      activeUpload = null;
    }
    try {
      await api("/api/logout", { method: "POST" });
    } catch {
      // ignore
    }

    me = null;
    pendingRecoveryCodes = null;
    uploading = false;
    seenIds.clear();
    messagesEl.replaceChildren();
    clearPending();
    closeLightbox();
    closeWs();
    setConnection("未连接", "offline");
    updateOnlineCount(0);
    if (adminLink) adminLink.classList.add("hidden");
    showAuthPanel("login");
    window.requestAnimationFrame(() => loginUsername?.focus({ preventScroll: true }));
  }

  logoutButtons.forEach((button) => button.addEventListener("click", logout));
  statusLogoutBtn?.addEventListener("click", logout);
  $("#totp-setup-logout")?.addEventListener("click", logout);
  $("#twofa-logout")?.addEventListener("click", logout);

  // ---------- Security panel (devices + password) ----------
  const securityPanel = $("#security-panel");
  const deviceList = $("#device-list");
  const deviceEmpty = $("#device-empty");
  const passwordForm = $("#password-form");
  const passwordError = $("#password-error");
  const passwordBtn = $("#password-btn");

  function openSecurityPanel() {
    if (!securityPanel || !canChat()) return;
    securityPanel.classList.remove("hidden");
    loadDevices();
  }

  function closeSecurityPanel() {
    securityPanel?.classList.add("hidden");
    clearFormError(passwordError, passwordForm);
    passwordForm?.reset();
  }

  $("#security-btn")?.addEventListener("click", openSecurityPanel);
  $("#mobile-security-btn")?.addEventListener("click", openSecurityPanel);
  $("#security-close")?.addEventListener("click", closeSecurityPanel);
  $("#security-backdrop")?.addEventListener("click", closeSecurityPanel);

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && securityPanel && !securityPanel.classList.contains("hidden")) {
      closeSecurityPanel();
    }
  });

  function shortLabel(label) {
    if (!label) return "未知设备";
    const s = String(label);
    if (s.length <= 48) return s;
    return `${s.slice(0, 45)}…`;
  }

  async function loadDevices() {
    if (!deviceList) return;
    deviceList.replaceChildren();
    try {
      const devices = await api("/api/security/devices");
      if (!devices.length) {
        deviceEmpty?.classList.remove("hidden");
        return;
      }
      deviceEmpty?.classList.add("hidden");
      for (const d of devices) {
        const li = document.createElement("li");
        li.className = "device-item";
        const meta = document.createElement("div");
        meta.className = "device-meta";
        const title = document.createElement("strong");
        title.textContent = d.current ? "当前设备" : shortLabel(d.label);
        const sub = document.createElement("small");
        const last = d.last_seen ? new Date(d.last_seen).toLocaleString() : "—";
        const exp = d.expires_at ? new Date(d.expires_at).toLocaleDateString() : "—";
        sub.textContent = d.current
          ? `最近活跃 ${last} · 有效至 ${exp}`
          : `${shortLabel(d.label)} · 最近 ${last}`;
        meta.append(title, sub);

        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "device-revoke";
        btn.textContent = d.current ? "吊销" : "吊销";
        btn.title = "吊销此设备信任";
        btn.addEventListener("click", async () => {
          if (!confirm(d.current ? "吊销当前设备后，下次登录需验证码。确定？" : "确定吊销该设备？")) return;
          btn.disabled = true;
          try {
            await api(`/api/security/devices/${encodeURIComponent(d.id)}`, { method: "DELETE" });
            showToast("已吊销设备", "success");
            await loadDevices();
          } catch (err) {
            showToast(err.message || "吊销失败");
            btn.disabled = false;
          }
        });

        li.append(meta, btn);
        deviceList.appendChild(li);
      }
    } catch (err) {
      deviceEmpty?.classList.remove("hidden");
      if (deviceEmpty) deviceEmpty.textContent = err.message || "加载失败";
    }
  }

  passwordForm?.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearFormError(passwordError, passwordForm);
    const current_password = $("#pw-current").value;
    const new_password = $("#pw-new").value;
    const new2 = $("#pw-new2").value;
    if (!current_password || !new_password) {
      showFormError(passwordError, passwordForm, "请填写完整");
      return;
    }
    if (new_password.length < 8) {
      showFormError(passwordError, passwordForm, "新密码至少 8 位");
      return;
    }
    if (new_password !== new2) {
      showFormError(passwordError, passwordForm, "两次新密码不一致");
      return;
    }
    setButtonLoading(passwordBtn, true);
    try {
      await api("/api/security/password", {
        method: "POST",
        body: JSON.stringify({ current_password, new_password }),
      });
      passwordForm.reset();
      showToast("密码已更新，其他设备信任已清除", "success");
      await loadDevices();
    } catch (err) {
      showFormError(passwordError, passwordForm, err.message || "修改失败");
    } finally {
      setButtonLoading(passwordBtn, false);
    }
  });

  // ---------- WebSocket ----------
  function wsUrl() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${location.host}/ws`;
  }

  function connectWs() {
    if (!me) return;

    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    const generation = ++wsGeneration;
    if (ws) {
      const previous = ws;
      ws = null;
      try { previous.close(); } catch { /* no-op */ }
    }

    setConnection(wsRetry ? "正在重新连接" : "正在连接", "connecting");

    let socket;
    try {
      socket = new WebSocket(wsUrl());
    } catch {
      scheduleReconnect(generation);
      return;
    }
    ws = socket;

    socket.addEventListener("open", () => {
      if (generation !== wsGeneration || socket !== ws) return;
      wsRetry = 0;
      setConnection("已连接", "connected");
    });

    socket.addEventListener("message", (event) => {
      if (generation !== wsGeneration || socket !== ws) return;
      let data;
      try {
        data = JSON.parse(event.data);
      } catch {
        return;
      }

      switch (data.type) {
        case "history": {
          const history = Array.isArray(data.messages) ? data.messages : [];
          messagesEl.replaceChildren();
          seenIds.clear();
          renderTimelineNote();
          history.forEach((message) => appendMessage(message, false));
          if (!messagesEl.querySelector(".msg")) renderEmptyState();
          scrollBottom(true);
          break;
        }
        case "message":
          appendMessage(data.message, true);
          break;
        case "presence":
          updateOnlineCount(data.online ?? 0);
          break;
        case "error":
          showToast(data.message || "消息处理失败");
          break;
        case "force_logout":
          if (!me || data.user_id === me.id) {
            showToast(data.reason || "你已被强制下线");
            logout();
          }
          break;
        default:
          break;
      }
    });

    socket.addEventListener("close", () => {
      if (generation !== wsGeneration || socket !== ws) return;
      ws = null;
      if (!me) return;
      setConnection("连接已中断，正在重试", "connecting");
      scheduleReconnect(generation);
    });

    socket.addEventListener("error", () => {
      if (generation !== wsGeneration) return;
      try { socket.close(); } catch { /* no-op */ }
    });
  }

  function scheduleReconnect(generation) {
    if (!me || generation !== wsGeneration) return;
    const delay = Math.min(1000 * 2 ** wsRetry, 15000);
    wsRetry += 1;
    reconnectTimer = window.setTimeout(connectWs, delay);
  }

  function closeWs() {
    wsGeneration += 1;
    wsRetry = 0;
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (ws) {
      const socket = ws;
      ws = null;
      try { socket.close(); } catch { /* no-op */ }
    }
  }

  function setConnection(label, state) {
    connectionLabel.textContent = label;
    [connectionDot, sidebarPresence].filter(Boolean).forEach((dot) => {
      dot.classList.remove("connecting", "offline");
      if (state === "connecting") dot.classList.add("connecting");
      if (state === "offline") dot.classList.add("offline");
    });
  }

  function updateOnlineCount(value) {
    const count = Math.max(0, Number(value) || 0);
    onlineCount.textContent = String(count);
    sidebarOnlineCount.textContent = String(count);
  }

  window.setInterval(() => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "ping" }));
    }
  }, 25000);

  // ---------- Messages UI ----------
  function renderTimelineNote() {
    const note = document.createElement("div");
    note.className = "timeline-note";
    note.innerHTML = `
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
        <path d="M12 22a9 9 0 1 0-9-9"/><path d="M3 4v6h6M12 7v6l4 2"/>
      </svg>
      <span>以下为最近 3 小时的消息</span>`;
    messagesEl.appendChild(note);
  }

  function renderEmptyState() {
    if (messagesEl.querySelector(".empty-state")) return;
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.innerHTML = `
      <span class="empty-visual" aria-hidden="true">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <path d="M5 5h14a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-8l-5 4v-4H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Z"/>
          <path d="M8 10h8M8 13h5"/>
        </svg>
      </span>
      <strong>共享空间已准备好</strong>
      <p>发送一条消息，或拖入文件开启这段对话。</p>`;
    messagesEl.appendChild(empty);
  }

  function appendMessage(message, animateScroll) {
    if (!message || !message.id || seenIds.has(message.id)) return;

    const wasNearBottom = isNearBottom();
    seenIds.add(message.id);
    messagesEl.querySelector(".empty-state")?.remove();

    const mine = Boolean(me && message.user_id === me.id);
    const isSystem = message.kind === "system";
    const row = document.createElement("div");
    row.className = `msg ${isSystem ? "system" : mine ? "out" : "in"}`;
    if (animateScroll) row.classList.add("message-enter");
    row.dataset.id = message.id;

    const bubble = document.createElement("div");
    bubble.className = "bubble";

    if (!isSystem && !mine) {
      const nick = document.createElement("div");
      nick.className = "nick";
      nick.textContent = message.nickname || "匿名";
      bubble.appendChild(nick);
    }

    if (message.kind === "image" && message.file) {
      bubble.classList.add("has-media");
      bubble.appendChild(renderImage(message.file));
    } else if (message.kind === "video" && message.file) {
      bubble.classList.add("has-media");
      bubble.appendChild(renderVideo(message.file));
    } else if (message.kind === "file" && message.file) {
      bubble.appendChild(renderFile(message.file));
    }

    if (message.content) {
      const text = document.createElement("div");
      text.className = "text";
      text.textContent = message.content;
      bubble.appendChild(text);
    }

    if (!isSystem) {
      const meta = document.createElement("div");
      meta.className = "meta";
      const time = document.createElement("span");
      time.textContent = formatTime(message.ts);
      meta.appendChild(time);
      if (mine) {
        const check = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        check.setAttribute("class", "meta-check");
        check.setAttribute("viewBox", "0 0 16 16");
        check.setAttribute("fill", "none");
        check.setAttribute("stroke", "currentColor");
        check.setAttribute("stroke-width", "1.6");
        check.setAttribute("aria-hidden", "true");
        check.innerHTML = '<path d="m2.5 8 2.4 2.4 4.2-4.6"/><path d="m6.8 9.9 1.3 1.3 5.4-5.8"/>';
        meta.appendChild(check);
      }
      bubble.appendChild(meta);
    }

    if (isSystem) {
      row.appendChild(bubble);
    } else {
      const content = document.createElement("div");
      content.className = "message-content";
      content.appendChild(bubble);

      if (!mine) {
        const avatar = document.createElement("span");
        const nickname = message.nickname || "匿名";
        avatar.className = "message-avatar";
        avatar.textContent = getInitials(nickname);
        avatar.style.setProperty("--avatar-hue", String(hueFor(nickname)));
        avatar.setAttribute("aria-hidden", "true");
        row.append(avatar, content);
      } else {
        row.appendChild(content);
      }
    }

    messagesEl.appendChild(row);

    if (animateScroll && (wasNearBottom || mine)) {
      scrollBottom(false);
    } else if (animateScroll && !mine) {
      scrollLatest.classList.remove("hidden");
    }
  }

  function renderImage(file) {
    const wrap = document.createElement("div");
    wrap.className = "media-wrap";
    wrap.tabIndex = 0;
    wrap.setAttribute("role", "button");
    wrap.setAttribute("aria-label", `预览 ${file.name || "图片"}`);
    const img = document.createElement("img");
    img.src = `/api/files/${encodeURIComponent(file.id)}`;
    img.alt = file.name || "图片";
    img.loading = "lazy";
    img.decoding = "async";
    const preview = () => openLightbox("image", img.src, file.name, wrap);
    wrap.addEventListener("click", preview);
    wrap.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        preview();
      }
    });
    wrap.appendChild(img);
    return wrap;
  }

  function renderVideo(file) {
    const wrap = document.createElement("div");
    wrap.className = "media-wrap";
    wrap.style.cursor = "default";
    const video = document.createElement("video");
    video.src = `/api/files/${encodeURIComponent(file.id)}`;
    video.controls = true;
    video.preload = "metadata";
    video.setAttribute("playsinline", "");
    video.addEventListener("dblclick", () => openLightbox("video", video.src, file.name, video));
    wrap.appendChild(video);
    return wrap;
  }

  function renderFile(file) {
    const card = document.createElement("div");
    card.className = "file-card";

    const icon = document.createElement("div");
    icon.className = "file-icon";
    const ext = (file.name || "").split(".").pop() || "file";
    icon.textContent = ext.slice(0, 4);

    const info = document.createElement("div");
    info.className = "file-info";
    const name = document.createElement("div");
    name.className = "file-name";
    name.textContent = file.name || "文件";
    name.title = file.name || "";
    const size = document.createElement("div");
    size.className = "file-size";
    size.textContent = formatSize(file.size);
    info.append(name, size);

    const link = document.createElement("a");
    link.className = "file-dl";
    link.href = `/api/files/${encodeURIComponent(file.id)}/download`;
    link.title = "下载文件";
    link.setAttribute("aria-label", `下载 ${file.name || "文件"}`);
    link.setAttribute("download", file.name || "file");
    link.innerHTML = `
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" aria-hidden="true">
        <path d="M12 3v12m-5-5 5 5 5-5M5 21h14"/>
      </svg>`;

    card.append(icon, info, link);
    return card;
  }

  function formatTime(timestamp) {
    try {
      const date = new Date(timestamp);
      if (Number.isNaN(date.getTime())) return "";
      return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    } catch {
      return "";
    }
  }

  function formatSize(value) {
    const bytes = Number(value);
    if (!Number.isFinite(bytes) || bytes < 0) return "";
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  function getInitials(value) {
    const name = String(value || "U").trim();
    if (!name) return "U";
    if (/[\u3400-\u9fff]/u.test(name)) return Array.from(name)[0];
    const words = name.split(/\s+/).filter(Boolean);
    if (words.length > 1) {
      return `${Array.from(words[0])[0] || ""}${Array.from(words[1])[0] || ""}`.toUpperCase();
    }
    return Array.from(name).slice(0, 2).join("").toUpperCase();
  }

  function hueFor(value) {
    let hash = 0;
    for (const char of String(value || "")) {
      hash = (hash * 31 + char.codePointAt(0)) >>> 0;
    }
    return 205 + (hash % 105);
  }

  function isNearBottom(threshold = 110) {
    return messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < threshold;
  }

  function scrollBottom(instant) {
    messagesEl.scrollTo({
      top: messagesEl.scrollHeight,
      behavior: instant || reducedMotion.matches ? "auto" : "smooth",
    });
    scrollLatest.classList.add("hidden");
  }

  messagesEl.addEventListener("scroll", () => {
    scrollLatest.classList.toggle("hidden", isNearBottom(80));
  }, { passive: true });

  scrollLatest.addEventListener("click", () => scrollBottom(false));

  // ---------- Composer ----------
  function autoResize() {
    messageInput.style.height = "22px";
    messageInput.style.height = `${Math.min(messageInput.scrollHeight, 140)}px`;
  }

  function updateComposerState() {
    sendBtn.disabled = uploading || (!pendingFile && !messageInput.value.trim());
    attachBtn.disabled = uploading;
    previewCancel.disabled = uploading;
  }

  messageInput.addEventListener("input", () => {
    autoResize();
    updateComposerState();
  });

  messageInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      send();
    }
  });

  sendBtn.addEventListener("click", send);

  async function send() {
    if (uploading) return;
    if (pendingFile) {
      await uploadFile(pendingFile);
      return;
    }

    const content = messageInput.value.trim();
    if (!content) return;

    messageInput.value = "";
    autoResize();
    updateComposerState();

    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "text", content }));
      return;
    }

    try {
      await api("/api/messages/text", {
        method: "POST",
        body: JSON.stringify({ content }),
      });
    } catch (err) {
      messageInput.value = content;
      autoResize();
      updateComposerState();
      showToast(err.message || "发送失败，请稍后重试");
    }
  }

  attachBtn.addEventListener("click", () => {
    if (!uploading) fileInput.click();
  });

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    fileInput.value = "";
    if (file) setPendingFile(file);
  });

  previewCancel.addEventListener("click", () => {
    if (!uploading) clearPending();
  });

  function setPendingFile(file) {
    pendingFile = file;
    previewLabel.textContent = `${file.name} · ${formatSize(file.size)}`;
    previewBar.classList.remove("hidden");
    messageInput.placeholder = "为文件添加说明…";
    updateComposerState();
    messageInput.focus({ preventScroll: true });
  }

  function clearPending() {
    pendingFile = null;
    previewBar.classList.add("hidden");
    previewLabel.textContent = "";
    messageInput.placeholder = "写点什么…";
    updateComposerState();
  }

  function uploadFile(file) {
    if (uploading) return Promise.resolve();
    uploading = true;
    updateComposerState();

    return new Promise((resolve) => {
      const caption = messageInput.value.trim();
      messageInput.value = "";
      autoResize();

      const form = new FormData();
      form.append("file", file, file.name);
      if (caption) form.append("caption", caption);

      const xhr = new XMLHttpRequest();
      activeUpload = xhr;
      xhr.open("POST", "/api/upload");
      xhr.withCredentials = true;

      uploadProgress.classList.remove("hidden");
      uploadBar.style.width = "0%";
      uploadText.textContent = `正在上传 ${file.name}`;

      xhr.upload.onprogress = (event) => {
        if (!event.lengthComputable) return;
        const percent = Math.round((event.loaded / event.total) * 100);
        uploadBar.style.width = `${percent}%`;
        uploadText.textContent = `正在上传 ${file.name} · ${percent}%`;
      };

      const finish = (success, message) => {
        activeUpload = null;
        uploading = false;
        uploadProgress.classList.add("hidden");
        if (success) {
          clearPending();
        } else {
          if (caption) messageInput.value = caption;
          autoResize();
          updateComposerState();
          if (message) showToast(message);
        }
        resolve();
      };

      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          finish(true);
          return;
        }
        let message = "上传失败，请稍后重试";
        try {
          message = JSON.parse(xhr.responseText).error || message;
        } catch { /* use fallback */ }
        finish(false, message);
      };

      xhr.onerror = () => finish(false, "网络连接异常，文件上传失败");
      xhr.onabort = () => finish(false, me ? "上传已取消" : "");
      xhr.send(form);
    });
  }

  // ---------- Lightbox ----------
  function openLightbox(kind, src, title, trigger) {
    lightboxTrigger = trigger || document.activeElement;
    lightboxBody.replaceChildren();

    if (kind === "image") {
      const img = document.createElement("img");
      img.src = src;
      img.alt = title || "图片预览";
      lightboxBody.appendChild(img);
    } else {
      const video = document.createElement("video");
      video.src = src;
      video.controls = true;
      video.autoplay = true;
      video.setAttribute("playsinline", "");
      lightboxBody.appendChild(video);
    }

    lightbox.classList.remove("hidden");
    lightboxClose.focus({ preventScroll: true });
  }

  function closeLightbox() {
    if (lightbox.classList.contains("hidden")) return;
    lightbox.classList.add("hidden");
    lightboxBody.replaceChildren();
    if (lightboxTrigger?.focus) lightboxTrigger.focus({ preventScroll: true });
    lightboxTrigger = null;
  }

  lightboxClose.addEventListener("click", closeLightbox);
  lightbox.addEventListener("click", (event) => {
    if (event.target === lightbox || event.target.classList.contains("lightbox-backdrop")) {
      closeLightbox();
    }
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeLightbox();
  });

  // ---------- Drag and drop ----------
  function isFileDrag(event) {
    return Array.from(event.dataTransfer?.types || []).includes("Files");
  }

  function canChat() {
    return Boolean(me && me.next_step === "chat");
  }

  document.addEventListener("dragenter", (event) => {
    if (!canChat() || !isFileDrag(event)) return;
    event.preventDefault();
    dragDepth += 1;
    chatSurface.classList.add("is-dragging");
  });

  document.addEventListener("dragover", (event) => {
    if (!canChat() || !isFileDrag(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });

  document.addEventListener("dragleave", (event) => {
    if (!canChat() || dragDepth === 0) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) chatSurface.classList.remove("is-dragging");
  });

  document.addEventListener("drop", (event) => {
    if (!canChat() || !isFileDrag(event)) return;
    event.preventDefault();
    dragDepth = 0;
    chatSurface.classList.remove("is-dragging");
    const file = event.dataTransfer?.files?.[0];
    if (file) setPendingFile(file);
  });

  window.addEventListener("blur", () => {
    dragDepth = 0;
    chatSurface.classList.remove("is-dragging");
  });

  // ---------- Toast ----------
  function showToast(message, type = "error") {
    const toast = document.createElement("div");
    toast.className = `toast ${type}`;
    toast.setAttribute("role", type === "error" ? "alert" : "status");
    toast.innerHTML = `
      <span class="toast-icon" aria-hidden="true">
        ${type === "success"
          ? '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m6 12 4 4 8-9"/></svg>'
          : '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 7v6m0 4h.01"/></svg>'}
      </span>`;
    const text = document.createElement("span");
    text.textContent = message;
    toast.appendChild(text);
    toastRegion.appendChild(toast);

    window.setTimeout(() => {
      toast.classList.add("is-leaving");
      toast.addEventListener("animationend", () => toast.remove(), { once: true });
      window.setTimeout(() => toast.remove(), 350);
    }, 3600);
  }

  // ---------- Public config (UI labels) ----------
  let publicConfig = { device_trust_days: null, invite_ttl_hours: 24, registration_open: true };

  function applyPublicConfig(cfg) {
    if (!cfg || typeof cfg !== "object") return;
    publicConfig = { ...publicConfig, ...cfg };
    const days = Number(publicConfig.device_trust_days);
    const trustLabel = $("#trust-device-label");
    if (trustLabel) {
      trustLabel.textContent = Number.isFinite(days) && days > 0
        ? `信任此设备 ${days} 天`
        : "信任此设备";
    }
  }

  async function loadPublicConfig() {
    try {
      const cfg = await api("/api/config");
      applyPublicConfig(cfg);
      if (cfg && cfg.device_trust_days != null) {
        console.info("[chat] device_trust_days =", cfg.device_trust_days);
      }
    } catch (err) {
      console.warn("[chat] failed to load /api/config", err);
      applyPublicConfig(publicConfig);
    }
  }

  // Boot: always load config first so 2FA label matches server env
  (async () => {
    await loadPublicConfig();
    await trySession();
  })();
})();
