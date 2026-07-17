(() => {
  "use strict";

  const $ = (sel) => document.querySelector(sel);

  const loginView = $("#login-view");
  const chatView = $("#chat-view");
  const loginForm = $("#login-form");
  const loginError = $("#login-error");
  const loginBtn = $("#login-btn");
  const messagesEl = $("#messages");
  const messageInput = $("#message-input");
  const sendBtn = $("#send-btn");
  const attachBtn = $("#attach-btn");
  const fileInput = $("#file-input");
  const logoutBtn = $("#logout-btn");
  const onlineCount = $("#online-count");
  const myName = $("#my-name");
  const previewBar = $("#preview-bar");
  const previewLabel = $("#preview-label");
  const previewCancel = $("#preview-cancel");
  const uploadProgress = $("#upload-progress");
  const uploadBar = $("#upload-bar");
  const uploadText = $("#upload-text");
  const lightbox = $("#lightbox");
  const lightboxBody = $("#lightbox-body");
  const lightboxClose = $("#lightbox-close");

  let me = null;
  let ws = null;
  let wsRetry = 0;
  let pendingFile = null;
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

  // ---------- Auth ----------
  async function trySession() {
    try {
      me = await api("/api/me");
      showChat();
      return true;
    } catch {
      showLogin();
      return false;
    }
  }

  function showLogin() {
    chatView.classList.add("hidden");
    loginView.classList.remove("hidden");
    closeWs();
  }

  function showChat() {
    loginView.classList.add("hidden");
    chatView.classList.remove("hidden");
    myName.textContent = me.nickname;
    connectWs();
    messageInput.focus();
  }

  loginForm.addEventListener("submit", async (e) => {
    e.preventDefault();
    loginError.classList.add("hidden");
    loginBtn.disabled = true;
    try {
      const data = await api("/api/login", {
        method: "POST",
        body: JSON.stringify({
          nickname: $("#nickname").value.trim(),
          password: $("#password").value,
        }),
      });
      me = data.user;
      $("#password").value = "";
      showChat();
    } catch (err) {
      loginError.textContent =
        err.status === 429
          ? "尝试次数过多，请稍后再试"
          : err.message === "未登录或会话已过期" || err.status === 401
            ? "密码错误"
            : err.message || "登录失败";
      loginError.classList.remove("hidden");
    } finally {
      loginBtn.disabled = false;
    }
  });

  logoutBtn.addEventListener("click", async () => {
    try {
      await api("/api/logout", { method: "POST" });
    } catch { /* ignore */ }
    me = null;
    seenIds.clear();
    messagesEl.innerHTML = "";
    showLogin();
  });

  // ---------- WebSocket ----------
  function wsUrl() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${location.host}/ws`;
  }

  function connectWs() {
    closeWs();
    ws = new WebSocket(wsUrl());

    ws.addEventListener("open", () => {
      wsRetry = 0;
    });

    ws.addEventListener("message", (ev) => {
      let data;
      try {
        data = JSON.parse(ev.data);
      } catch {
        return;
      }
      switch (data.type) {
        case "history":
          messagesEl.innerHTML = "";
          seenIds.clear();
          (data.messages || []).forEach((m) => appendMessage(m, false));
          scrollBottom(true);
          break;
        case "message":
          appendMessage(data.message, true);
          break;
        case "presence":
          onlineCount.textContent = String(data.online ?? 0);
          break;
        case "error":
          console.warn(data.message);
          break;
      }
    });

    ws.addEventListener("close", () => {
      ws = null;
      if (!me) return;
      const delay = Math.min(1000 * 2 ** wsRetry, 15000);
      wsRetry += 1;
      setTimeout(connectWs, delay);
    });

    ws.addEventListener("error", () => {
      try { ws.close(); } catch { /* */ }
    });
  }

  function closeWs() {
    if (ws) {
      const s = ws;
      ws = null;
      try { s.close(); } catch { /* */ }
    }
  }

  // Keepalive
  setInterval(() => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "ping" }));
    }
  }, 25000);

  // ---------- Messages UI ----------
  function appendMessage(m, animateScroll) {
    if (!m || !m.id || seenIds.has(m.id)) return;
    seenIds.add(m.id);

    const mine = me && m.user_id === me.id;
    const isSystem = m.kind === "system";

    const row = document.createElement("div");
    row.className = `msg ${isSystem ? "system" : mine ? "out" : "in"}`;
    row.dataset.id = m.id;

    const bubble = document.createElement("div");
    bubble.className = "bubble";

    if (!isSystem && !mine) {
      const nick = document.createElement("div");
      nick.className = "nick";
      nick.textContent = m.nickname || "匿名";
      bubble.appendChild(nick);
    }

    if (m.kind === "image" && m.file) {
      bubble.appendChild(renderImage(m.file));
    } else if (m.kind === "video" && m.file) {
      bubble.appendChild(renderVideo(m.file));
    } else if (m.kind === "file" && m.file) {
      bubble.appendChild(renderFile(m.file));
    }

    if (m.content) {
      const text = document.createElement("div");
      text.className = "text";
      text.textContent = m.content;
      bubble.appendChild(text);
    }

    if (!isSystem) {
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = formatTime(m.ts);
      bubble.appendChild(meta);
    }

    row.appendChild(bubble);
    messagesEl.appendChild(row);

    if (animateScroll) {
      const nearBottom =
        messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 120;
      if (nearBottom || mine) scrollBottom(false);
    }
  }

  function renderImage(file) {
    const wrap = document.createElement("div");
    wrap.className = "media-wrap";
    const img = document.createElement("img");
    img.src = `/api/files/${file.id}`;
    img.alt = file.name || "图片";
    img.loading = "lazy";
    img.addEventListener("click", () => openLightbox("image", img.src, file.name));
    wrap.appendChild(img);
    return wrap;
  }

  function renderVideo(file) {
    const wrap = document.createElement("div");
    wrap.className = "media-wrap";
    const video = document.createElement("video");
    video.src = `/api/files/${file.id}`;
    video.controls = true;
    video.preload = "metadata";
    video.addEventListener("dblclick", () => openLightbox("video", video.src, file.name));
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

    const a = document.createElement("a");
    a.className = "file-dl";
    a.href = `/api/files/${file.id}/download`;
    a.textContent = "下载";
    a.setAttribute("download", file.name || "file");

    card.append(icon, info, a);
    return card;
  }

  function formatTime(ts) {
    try {
      const d = new Date(ts);
      return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    } catch {
      return "";
    }
  }

  function formatSize(n) {
    if (n == null) return "";
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  function scrollBottom(instant) {
    messagesEl.scrollTo({
      top: messagesEl.scrollHeight,
      behavior: instant ? "auto" : "smooth",
    });
  }

  // ---------- Composer ----------
  function autoResize() {
    messageInput.style.height = "auto";
    messageInput.style.height = Math.min(messageInput.scrollHeight, 140) + "px";
  }

  messageInput.addEventListener("input", autoResize);

  messageInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });

  sendBtn.addEventListener("click", send);

  async function send() {
    if (pendingFile) {
      await uploadFile(pendingFile);
      return;
    }
    const content = messageInput.value.trim();
    if (!content) return;
    messageInput.value = "";
    autoResize();

    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "text", content }));
    } else {
      try {
        await api("/api/messages/text", {
          method: "POST",
          body: JSON.stringify({ content }),
        });
      } catch (err) {
        alert(err.message || "发送失败");
      }
    }
  }

  attachBtn.addEventListener("click", () => fileInput.click());

  fileInput.addEventListener("change", () => {
    const f = fileInput.files && fileInput.files[0];
    fileInput.value = "";
    if (!f) return;
    pendingFile = f;
    previewBar.classList.remove("hidden");
    previewLabel.textContent = `待发送: ${f.name} (${formatSize(f.size)})`;
    messageInput.placeholder = "可添加说明后点击发送…";
  });

  previewCancel.addEventListener("click", clearPending);

  function clearPending() {
    pendingFile = null;
    previewBar.classList.add("hidden");
    previewLabel.textContent = "";
    messageInput.placeholder = "输入消息...";
  }

  function uploadFile(file) {
    return new Promise((resolve) => {
      const caption = messageInput.value.trim();
      messageInput.value = "";
      autoResize();

      const fd = new FormData();
      fd.append("file", file, file.name);
      if (caption) fd.append("caption", caption);

      const xhr = new XMLHttpRequest();
      xhr.open("POST", "/api/upload");
      xhr.withCredentials = true;

      uploadProgress.classList.remove("hidden");
      uploadBar.style.width = "0%";
      uploadText.textContent = `上传 ${file.name}…`;

      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) {
          const pct = Math.round((e.loaded / e.total) * 100);
          uploadBar.style.width = pct + "%";
          uploadText.textContent = `上传中 ${pct}% — ${file.name}`;
        }
      };

      xhr.onload = () => {
        uploadProgress.classList.add("hidden");
        clearPending();
        if (xhr.status >= 200 && xhr.status < 300) {
          resolve();
        } else {
          let msg = "上传失败";
          try {
            msg = JSON.parse(xhr.responseText).error || msg;
          } catch { /* */ }
          alert(msg);
          resolve();
        }
      };

      xhr.onerror = () => {
        uploadProgress.classList.add("hidden");
        clearPending();
        alert("网络错误，上传失败");
        resolve();
      };

      xhr.send(fd);
    });
  }

  // ---------- Lightbox ----------
  function openLightbox(kind, src, title) {
    lightboxBody.innerHTML = "";
    if (kind === "image") {
      const img = document.createElement("img");
      img.src = src;
      img.alt = title || "";
      lightboxBody.appendChild(img);
    } else {
      const video = document.createElement("video");
      video.src = src;
      video.controls = true;
      video.autoplay = true;
      lightboxBody.appendChild(video);
    }
    lightbox.classList.remove("hidden");
  }

  function closeLightbox() {
    lightbox.classList.add("hidden");
    lightboxBody.innerHTML = "";
  }

  lightboxClose.addEventListener("click", closeLightbox);
  lightbox.addEventListener("click", (e) => {
    if (e.target === lightbox) closeLightbox();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeLightbox();
  });

  // ---------- Drag & drop ----------
  ["dragenter", "dragover"].forEach((ev) => {
    document.addEventListener(ev, (e) => {
      if (!me) return;
      e.preventDefault();
    });
  });
  document.addEventListener("drop", (e) => {
    if (!me) return;
    e.preventDefault();
    const f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    if (!f) return;
    pendingFile = f;
    previewBar.classList.remove("hidden");
    previewLabel.textContent = `待发送: ${f.name} (${formatSize(f.size)})`;
    messageInput.placeholder = "可添加说明后点击发送…";
  });

  // Boot
  trySession();
})();
