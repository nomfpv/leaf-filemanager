"use strict";

const api = globalThis.browser ?? globalThis.chrome;
const DEFAULTS = { enabled: true, noCookies: false, excluded: "" };

async function load() {
  const cfg = { ...DEFAULTS, ...(await api.storage.local.get(DEFAULTS)) };
  document.getElementById("enabled").checked = cfg.enabled;
  document.getElementById("noCookies").checked = cfg.noCookies;
  document.getElementById("excluded").value = cfg.excluded;
}

function save() {
  api.storage.local.set({
    enabled: document.getElementById("enabled").checked,
    noCookies: document.getElementById("noCookies").checked,
    excluded: document.getElementById("excluded").value,
  });
}

async function ping() {
  const status = document.getElementById("status");
  let r = null;
  try {
    r = await api.runtime.sendMessage({ type: "ping" });
  } catch {
    r = null;
  }
  if (r?.ok) {
    status.textContent = "leaf is running and connected.";
    status.className = "ok";
  } else if (r?.why === "not-running") {
    status.textContent = "leaf is connected but not running now. It is started when a download comes.";
    status.className = "ok";
  } else {
    status.textContent = "leaf cannot be reached. In leaf: Settings, Advanced, Downloads, Connect browsers.";
    status.className = "bad";
  }
}

for (const id of ["enabled", "noCookies"]) document.getElementById(id).addEventListener("change", save);
document.getElementById("excluded").addEventListener("input", save);
load();
ping();
