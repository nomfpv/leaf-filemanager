"use strict";

// The popup: "Browse through Mullvad" on, "Turn off (browse without VPN)" (the work is in background.js), and leaf's VPN state.
// The switch never turns itself off: only the button here does.

const api = globalThis.browser ?? globalThis.chrome;
const box = document.getElementById("mullvad");
const onRow = document.getElementById("onRow");
const offRow = document.getElementById("offRow");
const off = document.getElementById("off");
const state = document.getElementById("state");
const note = document.getElementById("note");
const info = document.getElementById("info");

function ask(type) {
  return api.runtime.sendMessage({ type }).catch(() => null);
}

function show(r, problem) {
  const on = !!r?.on;
  box.checked = false;
  onRow.hidden = on;
  offRow.hidden = !on;
  const vpn = r?.vpn;
  let text;
  let good = false;
  if (!vpn) text = "leaf cannot be reached (it is not running, or the browser is not connected to it).";
  else if (vpn.state === "on") {
    text = `leaf's VPN is on${vpn.country ? `, exit in ${vpn.country}` : ""}.`;
    good = true;
  } else if (vpn.state === "connecting") text = "leaf's VPN is connecting.";
  else text = "leaf's VPN is off.";
  if (on && good && !r?.note) text = `Browsing through Mullvad${vpn.country ? ` (${vpn.country})` : ""}.`;
  else if (on) good = false;
  state.textContent = text;
  state.className = good ? "ok" : "bad";
  note.textContent = problem || r?.note || (!on && r?.why) || "";
  // (Firefox with leaf's proxy password: the proxy is given per request, which Firefox does not offer for everything)
  info.textContent = r?.perRequest ? "Firefox's own sites (addons.mozilla.org, accounts.firefox.com) and its background requests do not go through Mullvad while leaf's proxy asks for a password." : "";
}

async function refresh(problem) {
  show(await ask("mullvad-state"), problem);
}

box.addEventListener("change", async () => {
  if (!box.checked) return;
  box.disabled = true;
  const r = await ask("mullvad-on");
  box.disabled = false;
  await refresh(r?.ok ? "" : (r?.why ?? "It did not work."));
});

off.addEventListener("click", async () => {
  off.disabled = true;
  await ask("mullvad-off");
  off.disabled = false;
  await refresh("");
});

document.getElementById("options").addEventListener("click", (e) => {
  e.preventDefault();
  api.runtime.openOptionsPage();
  window.close();
});

refresh("");

// "Download video with leaf": this tab's address goes to leaf (yt-dlp does the rest). "Use this browser's sign-in" is off until ticked.
const videoBtn = document.getElementById("video");
const signIn = document.getElementById("signIn");
const videoNote = document.getElementById("videoNote");
api.storage.local.get({ videoSignIn: false }).then((c) => {
  signIn.checked = !!c.videoSignIn;
}, () => {});
signIn.addEventListener("change", () => api.storage.local.set({ videoSignIn: signIn.checked }));
videoBtn.addEventListener("click", async () => {
  videoBtn.disabled = true;
  let tab = null;
  try {
    [tab] = await api.tabs.query({ active: true, currentWindow: true });
  } catch {
    tab = null;
  }
  const r = tab?.url ? await api.runtime.sendMessage({ type: "video", url: tab.url }).catch(() => null) : null;
  videoBtn.disabled = false;
  videoNote.textContent = r?.ok ? "Sent to leaf. Its choices are waiting there." : "Not sent: leaf could not take this page (is leaf connected? Settings, Downloads, Connect browsers).";
});
