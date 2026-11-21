// leaf browser capture: hands a download the browser starts to leaf (through the native host `leaf-nmhost`) and cancels the
// browser's own copy only after leaf has said it took it over. If leaf says no, or cannot be reached, the browser carries on.
//
// Adapted from AB Download Manager browser integration (https://github.com/amir1376/ab-download-manager-browser-integration,
// src/linkgrabber/DownloadLinkInterceptor.ts), Copyright AmirHossein Abdolmotallebi, Apache License 2.0 (LICENSE-APACHE-2.0).
// Changed for leaf: rewritten as plain JavaScript; only the request recording (onSendHeaders with extraHeaders) and the
// downloads.onCreated take-over are kept; media, HLS, link grabber, popup, content scripts and the HTTP backend are removed; the
// browser's download is paused while leaf answers and cancelled only on leaf's "ok" (resumed otherwise); the message is leaf's.

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

// the options page asks whether leaf can be reached
api.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg?.type !== "ping") return false;
  askLeaf({ v: 1, type: "ping" }).then((r) => reply(r ?? { v: 1, ok: false, why: "no-host" }));
  return true;
});
