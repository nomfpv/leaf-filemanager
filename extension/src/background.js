// leaf browser capture: hands a download the browser starts to leaf (through the native host `leaf-nmhost`) and cancels the
// browser's own copy only after leaf has said it took it over. If leaf says no, or cannot be reached, the browser carries on.
//
// Adapted from AB Download Manager browser integration (https://github.com/amir1376/ab-download-manager-browser-integration,
// src/linkgrabber/DownloadLinkInterceptor.ts), Copyright AmirHossein Abdolmotallebi, Apache License 2.0 (LICENSE-APACHE-2.0).
// Changed for leaf: rewritten as plain JavaScript; only the request recording (onSendHeaders with extraHeaders) and the
// downloads.onCreated take-over are kept; media, HLS, link grabber, popup, content scripts and the HTTP backend are removed; the
// browser's download is paused while leaf answers and cancelled only on leaf's "ok" (resumed otherwise); the message is leaf's.
// "Browse through Mullvad" (below) is leaf's own.

"use strict";

const api = globalThis.browser ?? globalThis.chrome;
const HOST = "dev.leaf.nmhost";
const IS_FIREFOX = api.runtime.getURL("").startsWith("moz-extension:");
// a download created longer ago than this is not a new one (a restored or retried item)
const FRESH_MS = 10_000;
// a finished request is kept this long so its download item can still find it
const KEEP_MS = 20_000;
// leaf-nmhost answers within 25 s, a start of leaf included (its DEADLINE), so leaf never takes a download the browser resumed
const ANSWER_MS = 30_000;
// the request headers leaf takes; everything else stays in the browser
const HEADERS = ["cookie", "authorization", "referer", "user-agent", "accept-language"];
const CREDENTIALS = ["cookie", "authorization"];

const DEFAULTS = { enabled: true, noCookies: false, excluded: "" };

// requestId -> { url, first, method, headers, page }
const requests = new Map();

function browserName() {
  if (IS_FIREFOX) return "firefox";
  const brands = globalThis.navigator?.userAgentData?.brands?.map((b) => b.brand) ?? [];
  if (brands.some((b) => /Edge/i.test(b))) return "edge";
  if (brands.some((b) => /Brave/i.test(b))) return "brave";
  return "chrome";
}

async function config() {
  try {
    return { ...DEFAULTS, ...(await api.storage.local.get(DEFAULTS)) };
  } catch {
    return DEFAULTS;
  }
}

function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

function isExcluded(url, list) {
  const host = hostOf(url);
  if (!host) return false;
  return list
    .split(/[\s,]+/)
    .map((s) => s.trim().toLowerCase().replace(/^\*\./, ""))
    .filter(Boolean)
    .some((site) => host === site || host.endsWith("." + site));
}

function remember(d) {
  const seen = requests.get(d.requestId);
  const headers = {};
  for (const h of d.requestHeaders ?? []) {
    if (h.value && HEADERS.includes(h.name.toLowerCase())) headers[h.name] = h.value;
  }
  // (a redirect is the same requestId: the last hop is what the download item names)
  requests.set(d.requestId, {
    first: seen?.first ?? d.url,
    url: d.url,
    method: d.method,
    headers,
    page: d.documentUrl ?? d.originUrl ?? seen?.page ?? d.initiator ?? null,
  });
}

function forget(d) {
  setTimeout(() => requests.delete(d.requestId), KEEP_MS);
}

function findRequest(...urls) {
  for (const r of requests.values()) {
    if (urls.includes(r.url) || urls.includes(r.first)) return r;
  }
  return null;
}

function askLeaf(msg) {
  const answer = api.runtime.sendNativeMessage(HOST, msg);
  const late = new Promise((resolve) => setTimeout(() => resolve(null), ANSWER_MS));
  return Promise.race([answer, late]).catch(() => null);
}

async function quietly(p) {
  try {
    await p;
  } catch {
    // the item may already be gone or finished
  }
}

async function takeOver(item) {
  const cfg = await config();
  if (!cfg.enabled) return;
  // a private / incognito window's download stays with the browser: leaf would keep its address in its download history
  if (item.incognito) return;
  const url = item.finalUrl || item.url;
  // blob:, data:, file: and the like stay with the browser
  if (!/^https?:\/\//i.test(url)) return;
  // started by another extension (or by this one)
  if (item.byExtensionId) return;
  if (item.state && item.state !== "in_progress") return;
  const started = Date.parse(item.startTime);
  if (!Number.isNaN(started) && Math.abs(Date.now() - started) > FRESH_MS) return;
  const req = findRequest(item.url, url);
  // a download the page did not request just now (the browser's own retry, a restored item): left alone
  if (!req) return;
  if (req.method && req.method !== "GET") return;
  const page = req.page || item.referrer || null;
  if (isExcluded(url, cfg.excluded) || (page && isExcluded(page, cfg.excluded))) return;
  const headers = { ...req.headers };
  if (cfg.noCookies) {
    for (const name of Object.keys(headers)) {
      if (CREDENTIALS.includes(name.toLowerCase())) delete headers[name];
    }
  }
  const size = item.fileSize > 0 ? item.fileSize : item.totalBytes > 0 ? item.totalBytes : -1;
  // the browser's copy waits while leaf answers
  await quietly(api.downloads.pause(item.id));
  const reply = await askLeaf({
    v: 1,
    type: "download",
    url,
    referrer: page,
    filename: item.filename || "",
    method: "GET",
    size,
    mime: item.mime || "",
    headers,
    browser: browserName(),
  });
  if (reply && reply.ok === true) {
    await quietly(api.downloads.cancel(item.id));
    await quietly(api.downloads.removeFile(item.id));
    await quietly(api.downloads.erase({ id: item.id }));
  } else {
    await quietly(api.downloads.resume(item.id));
  }
}

const filter = { urls: ["http://*/*", "https://*/*"], types: ["main_frame", "sub_frame", "other", "xmlhttprequest"] };
const sendOptions = ["requestHeaders"];
// Chromium hides Cookie and Referer unless asked; Firefox gives them and has no such option
if (!IS_FIREFOX) sendOptions.push("extraHeaders");

api.webRequest.onSendHeaders.addListener(remember, filter, sendOptions);
api.webRequest.onCompleted.addListener(forget, filter);
api.webRequest.onErrorOccurred.addListener(forget, filter);
api.downloads.onCreated.addListener((item) => {
  takeOver(item);
});

// ------------------------------------------------------------------------------------------------ Browse through Mullvad
//
// The browser's proxy is set to leaf's SOCKS5 proxy (Settings > VPN > Let other programs use the VPN), which sends everything
// through leaf's VPN and looks names up through it (remote DNS). leaf's proxy refuses every connection while the VPN is not up,
// so pages then fail instead of loading directly. When leaf says the VPN is off (or leaf is gone, or late), the proxy stays set,
// the badge turns red and one notice says so; only the user turns the switch off. WebRTC is kept to the proxy while it is on.

const VPN_ALARM = "leaf-vpn";
// (Chromium's shortest alarm period)
const VPN_EVERY_MIN = 0.5;
// the browser's own network and local names never go to the proxy
const BYPASS_CHROMIUM = ["<local>", "localhost", "127.0.0.0/8", "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "[::1]", "*.local"];
const BYPASS_FIREFOX = "localhost, 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, [::1], .local";
const VPN_DEFAULTS = { mullvad: false, mullvadProxy: null, mullvadNote: "", mullvadAlerted: false, mullvadCountry: "" };
const badge = api.action ?? api.browserAction;

// Firefox, when the proxy setting cannot be used (a password, or the add-on is not allowed in private windows): the proxy is
// given per request instead
let perRequest = null;

async function vpnConfig() {
  try {
    return { ...VPN_DEFAULTS, ...(await api.storage.local.get(VPN_DEFAULTS)) };
  } catch {
    return VPN_DEFAULTS;
  }
}

// leaf's answer: { state: off|connecting|on|failed, country, proxy: { host, port, user, password } | null }, or null when leaf
// cannot be reached (not running: it is not started for this)
async function vpnState() {
  const r = await askLeaf({ v: 1, type: "vpn" });
  return r?.ok === true && r.vpn ? r.vpn : null;
}

// leaf's proxy password is kept in memory only, never in the browser profile (storage.local is a file on disk)
let livePassword = null;

// what is kept of leaf's proxy in storage: its address and user name, not the password
function kept(p) {
  return { host: p.host, port: p.port, user: p.user ?? null };
}

// leaf's proxy `a` is the one set now (`b` from storage; the password compared with the one in memory)
function sameProxy(a, b) {
  return !!a && !!b && a.host === b.host && a.port === b.port && (a.user ?? null) === (b.user ?? null) && (a.password ?? null) === livePassword;
}

function isLocalHost(host) {
  const h = host.replace(/^\[|\]$/g, "");
  if (h === "localhost" || h === "::1" || h.endsWith(".local") || !h.includes(".")) return true;
  const m = h.match(/^(\d+)\.(\d+)\.\d+\.\d+$/);
  if (!m) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254);
}

function onProxyRequest(info) {
  // (nothing returned: the browser's own setting applies)
  if (!perRequest) return undefined;
  if (isLocalHost(hostOf(info.url))) return { type: "direct" };
  return perRequest;
}

if (IS_FIREFOX && api.proxy?.onRequest) {
  api.proxy.onRequest.addListener(onProxyRequest, { urls: ["http://*/*", "https://*/*", "ws://*/*", "wss://*/*"] });
}

async function controllable() {
  try {
    const s = await api.proxy.settings.get({});
    return s.levelOfControl !== "controlled_by_other_extensions" && s.levelOfControl !== "not_controllable";
  } catch {
    return true;
  }
}

async function setProxy(p) {
  livePassword = p.password ?? null;
  if (IS_FIREFOX) {
    if (!p.user) {
      try {
        await api.proxy.settings.set({ value: { proxyType: "manual", socks: `${p.host}:${p.port}`, socksVersion: 5, proxyDNS: true, passthrough: BYPASS_FIREFOX } });
        perRequest = null;
        return;
      } catch {
        // not allowed in private windows: per request instead
      }
    }
    perRequest = { type: "socks", host: p.host, port: p.port, username: p.user ?? undefined, password: p.password ?? undefined, proxyDNS: true };
    return;
  }
  // (Chromium's socks5 sends the names to the proxy: remote DNS)
  await api.proxy.settings.set({ value: { mode: "fixed_servers", rules: { singleProxy: { scheme: "socks5", host: p.host, port: p.port }, bypassList: BYPASS_CHROMIUM } }, scope: "regular" });
}

async function clearProxy() {
  perRequest = null;
  livePassword = null;
  await quietly(IS_FIREFOX ? api.proxy.settings.clear({}) : api.proxy.settings.clear({ scope: "regular" }));
}

// WebRTC would otherwise reach out over UDP past the proxy and show the real address
async function webRtcThroughProxy(on) {
  const policy = api.privacy?.network?.webRTCIPHandlingPolicy;
  if (!policy) return;
  await quietly(on ? policy.set({ value: "disable_non_proxied_udp" }) : policy.clear({}));
}

function showBadge(text, title) {
  if (!badge) return;
  quietly(badge.setBadgeText({ text }));
  quietly(badge.setBadgeBackgroundColor({ color: text === "VPN" ? "#1e7a3c" : "#b3261e" }));
  quietly(badge.setTitle({ title }));
}

function notify(message) {
  quietly(api.notifications?.create("leaf-vpn", { type: "basic", iconUrl: api.runtime.getURL("icons/leaf-128.png"), title: "leaf", message }));
}

// the reason Browse through Mullvad cannot be turned on now, or null
function cannotUse(vpn) {
  if (!vpn) return "leaf cannot be reached. Start leaf, connect its VPN, then try again.";
  if (!vpn.proxy) return "In leaf, turn on Settings > VPN > Let other programs use the VPN.";
  if (vpn.state !== "on") return "leaf's VPN is not connected. Connect it in leaf (Settings > VPN), then try again.";
  if (!IS_FIREFOX && vpn.proxy.user) return "This browser cannot give a SOCKS proxy a password. In leaf, turn off Settings > VPN > Ask for a password.";
  return null;
}

async function mullvadOn() {
  const vpn = await vpnState();
  const why = cannotUse(vpn);
  if (why) return { ok: false, why };
  if (!(await controllable())) return { ok: false, why: "Another extension controls this browser's proxy setting. Turn that one off first." };
  try {
    await setProxy(vpn.proxy);
  } catch (e) {
    return { ok: false, why: `The browser did not take the proxy setting (${e?.message ?? e}).` };
  }
  await webRtcThroughProxy(true);
  await api.storage.local.set({ mullvad: true, mullvadProxy: kept(vpn.proxy), mullvadNote: "", mullvadAlerted: false, mullvadCountry: vpn.country ?? "" });
  showBadge("VPN", `Browsing through Mullvad${vpn.country ? ` (${vpn.country})` : ""}`);
  api.alarms.create(VPN_ALARM, { periodInMinutes: VPN_EVERY_MIN });
  return { ok: true, country: vpn.country ?? "" };
}

// only the user turns it off (the popup's "Turn off (browse without VPN)")
async function mullvadOff() {
  await clearProxy();
  await webRtcThroughProxy(false);
  await api.storage.local.set({ mullvad: false, mullvadProxy: null, mullvadNote: "", mullvadAlerted: false, mullvadCountry: "" });
  await quietly(api.alarms.clear(VPN_ALARM));
  showBadge("", "leaf");
}

// The switch never turns itself off: whatever goes wrong, the browser's proxy stays pointed at leaf's (leaf refuses while its VPN
// is not up, a closed port fails; neither falls back to direct), so pages fail until the VPN is back or the user turns it off.
const HELD = "Pages fail until leaf's VPN is back or you turn Browse through Mullvad off ({why}).";

// one notice per outage, the red badge and the popup's note
async function hold(why) {
  const cfg = await vpnConfig();
  const note = HELD.replace("{why}", why);
  showBadge("!", note);
  await api.storage.local.set({ mullvadNote: note, mullvadAlerted: true });
  if (!cfg.mullvadAlerted) notify(note);
}

// looks at leaf's VPN again (every half minute while on, and when the browser starts)
async function checkVpn() {
  const cfg = await vpnConfig();
  if (!cfg.mullvad) return;
  const vpn = await vpnState();
  if (!vpn) return hold("leaf is not running or did not answer");
  if (!vpn.proxy) return hold("leaf's proxy for other programs is turned off");
  // a new port or password in leaf (or Firefox restarted and the per-request proxy is gone): set again
  if (!sameProxy(vpn.proxy, cfg.mullvadProxy) || (IS_FIREFOX && cfg.mullvadProxy?.user && !perRequest)) {
    try {
      await setProxy(vpn.proxy);
      await api.storage.local.set({ mullvadProxy: kept(vpn.proxy) });
    } catch {
      return hold("the browser did not take leaf's new proxy address");
    }
  }
  if (!IS_FIREFOX && vpn.proxy.user) return hold("leaf's proxy asks for a password, which this browser cannot give");
  if (vpn.state === "off" || vpn.state === "failed") return hold("leaf's VPN is not connected");
  if (vpn.state === "connecting") return hold("leaf's VPN is connecting");
  showBadge("VPN", `Browsing through Mullvad${vpn.country ? ` (${vpn.country})` : ""}`);
  await api.storage.local.set({ mullvadNote: "", mullvadAlerted: false, mullvadCountry: vpn.country ?? "" });
}

api.alarms.onAlarm.addListener((a) => {
  if (a.name === VPN_ALARM) checkVpn();
});
// (Firefox's background page, or the service worker, has just started, also at the browser's start before leaf runs: the alarm,
// the per-request proxy and the badge are set again; the proxy setting itself stays the browser's across restarts)
vpnConfig().then((cfg) => {
  // (an earlier version kept the password in storage: removed)
  if (cfg.mullvadProxy?.password) quietly(api.storage.local.set({ mullvadProxy: kept(cfg.mullvadProxy) }));
  if (!cfg.mullvad) return;
  api.alarms.get(VPN_ALARM).then((a) => a || api.alarms.create(VPN_ALARM, { periodInMinutes: VPN_EVERY_MIN }), () => {});
  checkVpn();
});

// the options page asks whether leaf can be reached; the popup turns Browse through Mullvad on and off
api.runtime.onMessage.addListener((msg, _sender, reply) => {
  const answer = (p) => {
    p.then(reply, (e) => reply({ ok: false, why: String(e?.message ?? e) }));
    return true;
  };
  switch (msg?.type) {
    case "ping":
      return answer(askLeaf({ v: 1, type: "ping" }).then((r) => r ?? { v: 1, ok: false, why: "no-host" }));
    case "mullvad-on":
      return answer(mullvadOn());
    case "mullvad-off":
      return answer(mullvadOff().then(() => ({ ok: true })));
    case "mullvad-state":
      return answer(
        (async () => {
          const cfg = await vpnConfig();
          const vpn = await vpnState();
          return { ok: true, on: cfg.mullvad, note: cfg.mullvadNote, vpn, why: cfg.mullvad ? null : cannotUse(vpn) };
        })(),
      );
    default:
      return false;
  }
});
