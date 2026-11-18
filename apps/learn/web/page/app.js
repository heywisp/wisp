const orb = document.getElementById("orb");
const pill = document.getElementById("pill");
const statuses = document.querySelectorAll("#pill > span");
const failure = document.getElementById("error");
const main = document.querySelector("main");
const blank = document.getElementById("frame");
const still = matchMedia("(prefers-reduced-motion: reduce)");
const sheet = document.getElementById("sheet");
const list = document.getElementById("rows");
const spend = document.getElementById("spend");
const title = document.getElementById("title");
const done = document.getElementById("done");
const foot = document.getElementById("spent");
const mic = document.getElementById("mic");
const FORMAT = { name: "pcm16_mono_48k", rate: 48000 };
const AWAKE = 10 * 60 * 1000;
const TOOL = "show_page";
const CAP = 65536;
const MARK = "board";
const SWELL = { ring: 1.2, core: 2 };
const LOUD = 0.2;
const DOT = 0.55;
const SOUND_SECONDS = 5;
const SOUND_WAIT = 10 * 1000;
const PEAK = 0.5;
const FADE = 0.1;
const WORDS = { brain: "Brain", voice: "Voice" };
const parts = Object.fromEntries(Object.keys(SWELL).map((part) => [part, orb.appendChild(document.createElementNS(orb.namespaceURI, "g"))]));
const sizes = {};
const calls = new Map();
let films = null;
let socket = null;
let playing = null;
let passing = null;
let timer = 0;
let rows = null;
let listing = null;
let asked = 0;
let saving = null;
let refused = null;
let thinking = false;
let elsewhere = null;
let mine = null;
let touched = 0;
let asleep = 0;
let board = null;
let theme = null;
let libraries = null;
let swelling = null;
let sound = null;

const handlers = {
  turn_started: () => change(() => (thinking = true)),
  turn_ended: () => change(() => (thinking = false)),
  voice: ({ event }) => change(() => (event.type === "started" ? started : ended)(event)),
  settings_changed: changed,
  spend_changed: (event) => (spend.textContent = event.total),
  rejected: (event) => answered(event.request, event.reason),
  tool_call_requested: ({ call_id, tool, input }) => tool === TOOL && calls.set(call_id, input),
  tool_call_started: ({ call_id }) => show(call_id, true, true),
  tool_call_finished: ({ call_id, outcome }) => show(call_id, outcome.status === "completed" && !outcome.is_error, false),
};

function post(command) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(command));
}

function describe() {
  if (sheet.open) post({ type: "describe_settings", request: ++asked });
}

function choose(item, group) {
  const row = listing;
  listing = null;
  if (!item.on) {
    const effort = Boolean(group.effort);
    saving = { row, effort, account: effort ? null : group.heading, request: ++asked };
    refused = null;
    post({ type: "change_settings", request: saving.request, save: true, changes: item.changes });
  }
  drawSheet(row);
}

function answered(request, reason) {
  if (request === undefined || saving?.request !== request) return;
  refused = reason ? { row: saving.row, reason } : null;
  saving = null;
  drawSheet();
}

function changed(event) {
  if (saving && saving.request === event.answers) saving.applied = true;
  describe();
}

function toned(words, tone) {
  const part = text("span", words);
  part.dataset.tone = tone;
  return part;
}

function named(name, ...after) {
  const line = text("span", text("span", name));
  line.className = "line";
  line.firstChild.className = "name";
  line.append(...after);
  return line;
}

function account({ name, kind, tone }) {
  return kind ? named(name, "\u00a0·\u00a0", toned(kind, tone)) : named(name);
}

function drawRow(key) {
  const row = rows[key];
  const button = document.createElement("button");
  Object.assign(button, { type: "button", className: "row", disabled: !row.groups.length });
  button.dataset.key = key;
  const mine = saving?.row === key ? saving : null;
  const model = mine && !mine.effort ? "saving…" : row.model ?? "none";
  const shown = toned(model, model === row.model ? "text" : "text.muted");
  shown.className = "name";
  button.append(text("span", WORDS[key]), shown);
  const heading = mine?.account ?? row.account;
  const under = heading ? [account(heading)] : [];
  if (row.effort) under.push(text("span", `effort ${mine?.effort ? "saving…" : row.effort}`));
  if (refused?.row === key) under.push(toned(`⨯ ${refused.reason}`, "error"));
  const [before, command, after] = heading?.sign_in ?? [];
  if (command) {
    const line = text("span", before);
    line.append(toned(command, "text"), after);
    under.push(line);
  }
  for (const line of under) line.classList.add("under");
  button.append(...under);
  button.onclick = () => {
    listing = key;
    drawSheet();
  };
  return button;
}

function drawList(row) {
  return row.groups.flatMap((group, g) => {
    const items = group.items.map((item, i) => {
      const button = document.createElement("button");
      Object.assign(button, { type: "button", className: "pick", disabled: Boolean(group.heading.expired) });
      button.dataset.key = `${listing}-${g}-${i}`;
      button.append(item.on ? named(item.shown, "\u00a0", toned("✓", "accent")) : named(item.shown));
      button.onclick = () => choose(item, group);
      return button;
    });
    return [text("h3", account(group.heading)), ...items];
  });
}

function drawSheet(from) {
  const focused = from ?? document.activeElement?.dataset.key;
  title.textContent = listing ? WORDS[listing] : "Settings";
  done.textContent = listing ? "Back" : "Done";
  foot.hidden = Boolean(listing);
  const parts = !rows ? [] : listing ? drawList(rows[listing]) : Object.keys(WORDS).map(drawRow);
  list.replaceChildren(...parts);
  list.querySelector(`[data-key="${focused}"]`)?.focus();
}

function text(tag, words) {
  const part = document.createElement(tag);
  part.append(words);
  return part;
}

function change(update) {
  update();
  render();
}

function render() {
  const live = Boolean(mine?.live);
  const muted = live && mine.muted;
  const state = !socket ? "offline" : live && mine.loud ? "answering" : thinking ? "thinking" : muted ? "muted" : live || elsewhere ? "listening" : "ready";
  const status = elsewhere && socket ? "another" : state;
  for (const shown of statuses) shown.classList.toggle("shown", shown.dataset.state === status);
  if (status !== state || !socket) failure.hidden = true;
  pill.hidden = !failure.hidden;
  mic.hidden = !live;
  mic.classList.toggle("muted", muted);
  mic.setAttribute("aria-label", muted ? "Unmute" : "Mute");
  swelling = { listening: live ? "ring" : null, answering: "core" }[state] ?? null;
  const film = passing ?? state;
  if (film !== playing) play(film);
}

function draw(dots) {
  for (const group of Object.values(parts)) group.replaceChildren();
  for (const [x, y, colour, part] of dots) {
    const dot = document.createElementNS(orb.namespaceURI, "circle");
    dot.setAttribute("cx", 1 + 2 * x);
    dot.setAttribute("cy", 1 + 2 * y);
    dot.setAttribute("r", DOT / (sizes[part] ?? 1));
    dot.setAttribute("fill", colour);
    parts[part].append(dot);
  }
}

function grow(part, size) {
  sizes[part] = size;
  parts[part].setAttribute("transform", `translate(6 4) scale(${size}) translate(-6 -4)`);
  for (const dot of parts[part].children) dot.setAttribute("r", DOT / size);
}

function swell(own, then) {
  requestAnimationFrame((now) => {
    const going = mine === own && own.live;
    for (const part of Object.keys(parts)) {
      const heard = going && !still.matches && swelling === part ? Math.min(LOUD, own.heard[part] ?? 0) : 0;
      const eased = own.eased[part] ?? 0;
      const step = ((now - (then ?? now)) * LOUD) / (heard > eased ? 50 : 300);
      own.eased[part] = eased + Math.max(-step, Math.min(step, heard - eased));
      const size = going ? 1 + ((SWELL[part] - 1) * own.eased[part]) / LOUD : 1;
      if (size !== sizes[part]) grow(part, size);
    }
    if (going) swell(own, now);
  });
}

function mute(own) {
  own.muted = !own.muted;
  for (const track of own.stream.getAudioTracks()) track.enabled = !own.muted;
}

function play(name) {
  clearTimeout(timer);
  playing = name;
  if (!films) return;
  const offline = name === "offline";
  const film = films[offline || still.matches ? "reduced" : "full"][offline ? "ready" : name];
  let lap = performance.now();
  const frame = (i) => {
    draw(film.frames[i].dots);
    let next = i + 1;
    if (next === film.frames.length) {
      if (film.once) timer = setTimeout(() => change(() => (passing = null)), lap + film.length - performance.now());
      if (film.once || next === 1) return;
      next = 0;
      lap += film.length;
    }
    if (document.hidden) return;
    timer = setTimeout(() => frame(next), lap + film.frames[next].at - performance.now());
  };
  frame(0);
}

function send(command) {
  socket?.send(JSON.stringify({ type: "voice", command }));
}

function tap() {
  if (!socket || elsewhere || (mine && !mine.live)) return;
  if (mine) return send({ type: "stop", call: mine.call });
  failure.hidden = true;
  start();
}

async function start() {
  const own = { call: crypto.randomUUID(), asking: true, live: false, loud: false, muted: false, heard: {}, eased: {} };
  mine = own;
  render();
  let kind = "start";
  try {
    const context = (own.context = new AudioContext({ sampleRate: FORMAT.rate }));
    own.stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    }).catch((error) => {
      kind = "microphone";
      throw error;
    });
    await context.audioWorklet.addModule("/audio.js");
    const capture = new AudioWorkletNode(context, "capture", { numberOfOutputs: 0 });
    own.playback = new AudioWorkletNode(context, "play", { numberOfInputs: 0, outputChannelCount: [1] });
    context.createMediaStreamSource(own.stream).connect(capture);
    own.playback.connect(context.destination);
    capture.port.onmessage = ({ data }) => mine === own && (typeof data === "number" ? (own.heard.ring = data) : own.live && socket?.send(data));
    own.playback.port.onmessage = ({ data }) => mine === own && (typeof data === "number" ? (own.heard.core = data) : change(() => louder(data)));
    await context.resume();
  } catch {
    const shown = mine === own;
    hangUp(own);
    return shown && change(() => fail(kind, ""));
  }
  if (mine !== own || document.hidden) return hangUp(own);
  own.asking = false;
  send({ type: "start", call: own.call, format: FORMAT.name });
}

function louder(loud) {
  if (loud && !mine.loud) passing = "chunk";
  mine.loud = loud;
  if (loud && sound?.source) quiet(FADE);
  playSound();
}

function queue(samples) {
  const peak = samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);
  if (!mine?.live || samples.length > SOUND_SECONDS * FORMAT.rate || !Number.isFinite(peak)) return;
  quiet();
  sound = { samples: peak > PEAK ? samples.map((sample) => (sample * PEAK) / peak) : samples, at: performance.now() };
  playSound();
}

function playSound() {
  if (!sound || sound.source || !mine?.live || mine.loud) return;
  if (performance.now() - sound.at > SOUND_WAIT) return (sound = null);
  const { context } = mine;
  const buffer = context.createBuffer(1, sound.samples.length, FORMAT.rate);
  buffer.copyToChannel(sound.samples, 0);
  const playing = Object.assign(sound, { source: new AudioBufferSourceNode(context, { buffer }), gain: new GainNode(context) });
  playing.source.connect(playing.gain).connect(context.destination);
  playing.source.onended = () => sound === playing && (sound = null);
  playing.source.start();
}

function quiet(fade = 0) {
  const { source, gain } = sound ?? {};
  sound = null;
  if (!source) return;
  const now = source.context.currentTime;
  if (fade) gain.gain.setTargetAtTime(0, now, fade / 4);
  source.stop(now + fade);
}

function started({ call }) {
  if (mine?.call !== call) {
    elsewhere = call;
  } else {
    mine.live = true;
    swell(mine);
    awake();
  }
}

function ended({ call, outcome }) {
  if (elsewhere === call) elsewhere = null;
  if (mine?.call !== call) return;
  const live = mine.live;
  hangUp();
  if (outcome.type === "failed") fail(live ? "stop" : "start", outcome.message);
  else if (live) passing = "stopped";
}

function hangUp(own = mine) {
  if (!own) return;
  if (mine === own) {
    mine = null;
    quiet();
  }
  own.stream?.getTracks().forEach((track) => track.stop());
  own.context?.close();
  own.lock?.release();
}

function fail(kind, why) {
  for (const part of failure.querySelectorAll("[data-for]")) part.hidden = !part.dataset.for.split(" ").includes(kind);
  document.getElementById("why").textContent = why;
  failure.hidden = false;
}

async function awake() {
  const fresh = performance.now() - touched < AWAKE;
  if (!mine?.live || mine.lock || document.hidden || !fresh || !navigator.wakeLock) return;
  const call = mine.call;
  const lock = await navigator.wakeLock.request("screen").catch(() => null);
  if (mine?.call !== call || mine.lock) return lock?.release();
  mine.lock = lock;
  lock?.addEventListener("release", () => {
    if (mine?.lock !== lock) return;
    mine.lock = null;
    awake();
  });
}

function letGo() {
  const lock = mine?.lock;
  if (mine) mine.lock = null;
  lock?.release();
}

function touch(event) {
  if (event.isTrusted) woke();
}

function woke() {
  touched = performance.now();
  clearTimeout(asleep);
  asleep = setTimeout(letGo, AWAKE);
  awake();
}

async function load() {
  const response = await fetch("/orb.json");
  films = await response.json();
  theme = await fetch("/theme.css").then((r) => r.text()).catch(() => "");
  libraries = await fetched("/lib/map.json").catch(() => ({}));
  play(playing);
  deliver();
}

async function fetched(path) {
  const map = await fetch(path).then((r) => r.json());
  const files = Object.entries(map).map(async ([name, file]) => [name, await fetch(file).then((r) => r.text())]);
  return Object.fromEntries(await Promise.all(files));
}

function show(call, ok, live) {
  const input = calls.get(call);
  calls.delete(call);
  if (!input || !ok) return;
  if (input.part === undefined || !board) reframe();
  if (sessionStorage.getItem(MARK) === call) return;
  board.queue.push({ call, input, live });
  deliver();
}

function reframe() {
  quiet();
  const frame = blank.content.firstElementChild.cloneNode();
  if (board) board.frame.replaceWith(frame);
  else main.append(frame);
  board = { frame, queue: [], ready: false, themed: false };
}

function deliver() {
  if (!board?.ready || !board.queue.length || libraries === null) return;
  const { call, live, input: { html, part } } = board.queue.shift();
  board.ready = false;
  sessionStorage.setItem(MARK, call);
  const first = board.themed ? {} : { theme, libraries };
  board.frame.contentWindow.postMessage({ html, part, call: live ? call : undefined, ...first }, "*");
  board.themed = true;
}

function heard({ source, data }) {
  if (!board || source !== board.frame.contentWindow) return;
  if (data?.touched) return woke();
  if (typeof data?.tell === "string") return navigator.userActivation?.isActive && note(data.tell);
  if (data?.report) return report(data.report, data.images, data.call);
  if (data?.sound instanceof Float32Array) return queue(data.sound);
  if (!data?.shown) return;
  sessionStorage.removeItem(MARK);
  board.ready = true;
  deliver();
}

function report(found, images = [], call) {
  if (!call || !(mine?.live || !elsewhere)) return;
  const command = { type: "screen_report", call_id: call, report: found, images };
  const size = () => new TextEncoder().encode(JSON.stringify(command)).length;
  if (size() > CAP) command.images = [];
  if (size() > CAP) found.errors = [];
  post(command);
}

function note(text) {
  send({ type: "note", call: mine?.call ?? elsewhere ?? "", text });
}

function cleared() {
  board?.frame.remove();
  board = null;
  calls.clear();
}

function connect() {
  const address = new URL("/socket", location.href);
  address.protocol = address.protocol === "https:" ? "wss:" : "ws:";
  const opened = new WebSocket(address);
  opened.binaryType = "arraybuffer";
  opened.onopen = () => {
    if (!films) load();
    cleared();
    change(() => (socket = opened));
  };
  opened.onmessage = ({ data }) => {
    if (typeof data !== "string") return mine?.live && mine.playback.port.postMessage(data, [data]);
    const event = JSON.parse(data);
    if (event.rows) {
      rows = event.rows;
      if (saving?.applied) saving = null;
      drawSheet();
    } else if (Object.hasOwn(handlers, event.type)) {
      handlers[event.type](event);
    }
  };
  opened.onclose = () => {
    hangUp();
    saving = null;
    change(() => ([socket, elsewhere, thinking] = [null, null, false]));
    setTimeout(connect, 2000);
  };
}

document.getElementById("gear").onclick = () => {
  sheet.showModal();
  describe();
};
done.onclick = () => {
  if (!listing) return sheet.close();
  const row = listing;
  listing = null;
  drawSheet(row);
};
sheet.addEventListener("close", () => {
  listing = null;
  drawSheet();
});
mic.onclick = () => mine?.live && change(() => mute(mine));
for (const part of [orb, pill, failure]) part.addEventListener("click", tap);
document.addEventListener("pointerdown", touch, true);
addEventListener("message", heard);
document.addEventListener("visibilitychange", () => {
  if (document.hidden && mine && !mine.asking) send({ type: "stop", call: mine.call });
  play(playing);
});
still.addEventListener("change", () => play(playing));
connect();
