(() => {
  "use strict";

  const $ = (selector, root = document) => root.querySelector(selector);
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  const loginView = $("#login-view");
  const chatView = $("#chat-view");
  const loginForm = $("#login-form");
  const loginError = $("#login-error");
  const loginBtn = $("#login-btn");
  const nicknameInput = $("#nickname");
  const passwordInput = $("#password");
  const passwordToggle = $("#password-toggle");

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

  async function trySession() {
    try {
      me = await api("/api/me");
      showChat(false);
      return true;
    } catch {
      me = null;
      showLogin(false);
      return false;
    }
  }

  function showLogin(animate = true) {
    closeWs();
    setConnection("未连接", "offline");
    updateOnlineCount(0);
    swapView("login", animate);
    window.requestAnimationFrame(() => nicknameInput.focus({ preventScroll: true }));
  }

  function showChat(animate = true) {
    const nickname = me?.nickname || "访客";
    myName.textContent = nickname;
    sideAvatar.textContent = getInitials(nickname);
    sideAvatar.style.setProperty("--avatar-hue", String(hueFor(nickname)));
    swapView("chat", animate);
    updateComposerState();
    connectWs();
    window.requestAnimationFrame(() => messageInput.focus({ preventScroll: true }));
  }

  function setLoginLoading(value) {
    loginBtn.disabled = value;
    loginBtn.classList.toggle("is-loading", value);
    loginBtn.setAttribute("aria-busy", String(value));
  }

  function showLoginError(message) {
    loginError.textContent = message;
    loginError.classList.remove("hidden");
    loginForm.classList.remove("login-form-error");
    void loginForm.offsetWidth;
    loginForm.classList.add("login-form-error");
  }

  function clearLoginError() {
    loginError.classList.add("hidden");
    loginForm.classList.remove("login-form-error");
  }

  loginForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearLoginError();

    const nickname = nicknameInput.value.trim();
    const password = passwordInput.value;
    if (!nickname || !password) {
      showLoginError(!nickname ? "请先输入你的昵称" : "请输入访问密码");
      (!nickname ? nicknameInput : passwordInput).focus();
      return;
    }

    setLoginLoading(true);
    try {
      const data = await api("/api/login", {
        method: "POST",
        body: JSON.stringify({ nickname, password }),
      });
      me = data.user;
      passwordInput.value = "";
      showChat(true);
    } catch (err) {
      const message = err.status === 429
        ? "尝试次数过多，请稍后再试"
        : err.message === "未登录或会话已过期" || err.status === 401
          ? "访问密码不正确，请重新输入"
          : err.message || "登录失败，请稍后重试";
      showLoginError(message);
      passwordInput.select();
    } finally {
      setLoginLoading(false);
    }
  });

  [nicknameInput, passwordInput].forEach((input) => {
    input.addEventListener("input", clearLoginError);
  });

  passwordToggle.addEventListener("click", () => {
    const willShow = passwordInput.type === "password";
    passwordInput.type = willShow ? "text" : "password";
    passwordToggle.classList.toggle("is-visible", willShow);
    passwordToggle.setAttribute("aria-pressed", String(willShow));
    passwordToggle.setAttribute("aria-label", willShow ? "隐藏密码" : "显示密码");
    passwordInput.focus({ preventScroll: true });
  });

  async function logout() {
    if (activeUpload) {
      activeUpload.abort();
      activeUpload = null;
    }
    try {
      await api("/api/logout", { method: "POST" });
    } catch {
      // The local session is cleared even if the request cannot complete.
    }

    me = null;
    uploading = false;
    seenIds.clear();
    messagesEl.replaceChildren();
    clearPending();
    closeLightbox();
    showLogin(true);
  }

  logoutButtons.forEach((button) => button.addEventListener("click", logout));

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

  document.addEventListener("dragenter", (event) => {
    if (!me || !isFileDrag(event)) return;
    event.preventDefault();
    dragDepth += 1;
    chatSurface.classList.add("is-dragging");
  });

  document.addEventListener("dragover", (event) => {
    if (!me || !isFileDrag(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });

  document.addEventListener("dragleave", (event) => {
    if (!me || dragDepth === 0) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) chatSurface.classList.remove("is-dragging");
  });

  document.addEventListener("drop", (event) => {
    if (!me || !isFileDrag(event)) return;
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

  // Boot
  trySession();
})();
