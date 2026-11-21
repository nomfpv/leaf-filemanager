"use strict";

// The popup: "Browse through Mullvad" on and off (the work is in background.js) and leaf's VPN state.

const api = globalThis.browser ?? globalThis.chrome;
const box = document.getElementById("mullvad");
const state = document.getElementById("state");
const note = document.getElementById("note");

function ask(type) {
  return api.runtime.sendMessage({ type }).catch(() => null);
}

function show(r, problem) {
  box.checked = !!r?.on;
  const vpn = r?.vpn;
  let text;
  let good = false;
  if (!vpn) text = "leaf cannot be reached (it is not running, or the browser is not connected to it).";
  else if (vpn.state === "on") {
    text = `leaf's VPN is on${vpn.country ? `, exit in ${vpn.country}` : ""}.`;
    good = true;
  } else if (vpn.state === "connecting") text = "leaf's VPN is connecting.";
  else text = "leaf's VPN is off.";
  if (r?.on && good) text = `Browsing through Mullvad${vpn.country ? ` (${vpn.country})` : ""}.`;
  state.textContent = text;
  state.className = good ? "ok" : "bad";
  note.textContent = problem || r?.note || (!r?.on && r?.why) || "";
}

async function refresh(problem) {
  show(await ask("mullvad-state"), problem);
}

box.addEventListener("change", async () => {
  box.disabled = true;
  const r = await ask(box.checked ? "mullvad-on" : "mullvad-off");
  box.disabled = false;
  await refresh(r?.ok ? "" : (r?.why ?? "It did not work."));
});

document.getElementById("options").addEventListener("click", (e) => {
  e.preventDefault();
  api.runtime.openOptionsPage();
  window.close();
});

refresh("");
