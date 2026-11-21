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
