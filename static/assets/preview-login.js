(async () => {
  await fetch("/api/login", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ nickname: "设计预览", password: "codex-ui-preview" }),
  });
  await fetch("/api/messages/text", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content: "新的界面已准备好，试试发送一条消息吧 ✨" }),
  });
  location.replace("/");
})();
